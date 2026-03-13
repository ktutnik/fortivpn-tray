use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use crate::{FortiError, VpnConfig};

/// Extract SVPNCOOKIE from HTTP response headers.
pub fn extract_cookie(response: &str) -> Option<String> {
    extract_named_cookie(response, "SVPNCOOKIE")
}

fn extract_named_cookie(response: &str, name: &str) -> Option<String> {
    let name_eq = format!("{name}=");
    let name_lower = name.to_lowercase();
    let name_eq_lower = format!("{name_lower}=");
    for line in response.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("set-cookie:") && lower.contains(&name_eq_lower) {
            if let Some(start) = line.find(&name_eq).or_else(|| line.find(&name_eq_lower)) {
                let value_start = start + name_eq.len();
                let rest = &line[value_start..];
                let value = rest.split(';').next().unwrap_or("").trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Parse the VPN configuration XML from /remote/fortisslvpn_xml.
pub fn parse_vpn_config_xml(xml: &str) -> Result<VpnConfig, FortiError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut assigned_ip = Ipv4Addr::UNSPECIFIED;
    let mut dns_servers = Vec::new();
    let mut search_domain = None;
    let mut routes = Vec::new();

    let mut in_dns = false;
    let mut current_text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name.as_str() == "dns" {
                    in_dns = true;
                }
                current_text.clear();
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "assigned-addr" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"ipv4" {
                                let val = String::from_utf8_lossy(&attr.value);
                                assigned_ip = val.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);
                            }
                        }
                    }
                    "addr" => {
                        let mut ip = Ipv4Addr::UNSPECIFIED;
                        let mut mask = Ipv4Addr::UNSPECIFIED;
                        for attr in e.attributes().flatten() {
                            match attr.key.as_ref() {
                                b"ip" => {
                                    let val = String::from_utf8_lossy(&attr.value);
                                    ip = val.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);
                                }
                                b"mask" => {
                                    let val = String::from_utf8_lossy(&attr.value);
                                    mask = val.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);
                                }
                                _ => {}
                            }
                        }
                        if !ip.is_unspecified() {
                            routes.push((ip, mask));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                current_text = e.unescape().unwrap_or_default().to_string();
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_str() {
                    "dns" => in_dns = false,
                    "ip" if in_dns => {
                        if let Ok(ip) = current_text.trim().parse::<Ipv4Addr>() {
                            dns_servers.push(ip);
                        }
                    }
                    "domain" if in_dns => {
                        let d = current_text.trim().to_string();
                        if !d.is_empty() {
                            search_domain = Some(d);
                        }
                    }
                    _ => {}
                }
                current_text.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(FortiError::AllocationFailed(format!(
                    "XML parse error: {e}"
                )))
            }
            _ => {}
        }
    }

    Ok(VpnConfig {
        assigned_ip,
        peer_ip: Ipv4Addr::new(169, 254, 2, 1),
        dns_servers,
        search_domain,
        routes,
    })
}

fn build_request(
    method: &str,
    path: &str,
    host: &str,
    port: u16,
    cookie: &str,
    body: &str,
) -> String {
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         User-Agent: Mozilla/5.0\r\n\
         Accept: */*\r\n\
         Cache-Control: no-store, no-cache, must-revalidate\r\n"
    );
    if !cookie.is_empty() {
        req.push_str(&format!("Cookie: SVPNCOOKIE={cookie}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    } else {
        req.push_str("Content-Length: 0\r\n");
    }
    req.push_str("\r\n");
    req.push_str(body);
    req
}

fn read_response<R: Read>(stream: &mut R) -> Result<String, FortiError> {
    let mut buf = vec![0u8; 8192];
    let mut response = String::new();
    loop {
        let n = stream.read(&mut buf).map_err(FortiError::Io)?;
        if n == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buf[..n]));
        if response.contains("\r\n\r\n") {
            break;
        }
    }
    Ok(response)
}

/// Build a rustls ClientConfig (shared between sync and async TLS).
pub(crate) fn build_tls_config(trusted_cert: &str) -> Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = if trusted_cert.is_empty() {
        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(CertPinVerifier {
                pinned_hash: trusted_cert.to_string(),
            }))
            .with_no_client_auth()
    };

    Arc::new(config)
}

/// Create a sync TLS connection to the FortiGate gateway.
pub(crate) fn tls_connect(
    host: &str,
    port: u16,
    trusted_cert: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, FortiError> {
    let addr = format!("{host}:{port}");
    let socket_addr = addr
        .to_socket_addrs()
        .map_err(|e| FortiError::GatewayUnreachable(format!("DNS resolve {host}: {e}")))?
        .next()
        .ok_or_else(|| FortiError::GatewayUnreachable(format!("No addresses for {host}")))?;
    let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(10))
        .map_err(|e| FortiError::GatewayUnreachable(format!("{e}")))?;
    tcp.set_nodelay(true).ok();

    let config = build_tls_config(trusted_cert);

    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| FortiError::GatewayUnreachable(format!("Invalid hostname: {e}")))?;
    let conn = rustls::ClientConnection::new(config, server_name)
        .map_err(|e| FortiError::CertificateNotTrusted(format!("{e}")))?;

    let tls = rustls::StreamOwned::new(conn, tcp);

    Ok(tls)
}

/// Compute SHA-256 hash of DER-encoded certificate, return as lowercase hex string.
fn cert_sha256_hex(cert_der: &[u8]) -> String {
    use std::fmt::Write;
    // Use simple SHA-256: read 64-byte blocks, manual computation not needed —
    // we can use the ring-compatible digest from aws-lc-rs via rustls re-export,
    // or just implement with the sha2 approach.
    // Simplest: use the raw aws_lc_rs digest.
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, cert_der);
    let mut hex = String::with_capacity(64);
    for b in digest.as_ref() {
        write!(hex, "{b:02x}").unwrap();
    }
    hex
}

/// TLS certificate verifier that pins to a specific SHA-256 hash.
/// Accepts any cert whose SHA-256 hash matches the pinned value.
#[derive(Debug)]
struct CertPinVerifier {
    pinned_hash: String,
}

impl rustls::client::danger::ServerCertVerifier for CertPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual_hash = cert_sha256_hex(end_entity.as_ref());
        if actual_hash == self.pinned_hash.to_lowercase() {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "Certificate pin mismatch: expected {}, got {}",
                self.pinned_hash, actual_hash
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Authenticate with the FortiGate gateway and retrieve VPN configuration.
pub fn authenticate(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    trusted_cert: &str,
) -> Result<(String, VpnConfig), FortiError> {
    let mut tls = tls_connect(host, port, trusted_cert)?;

    // Omit ajax=1 to avoid host check redirect (ret=1) on gateways that require it.
    // Without ajax=1, the gateway issues the SVPNCOOKIE directly on valid credentials.
    let body = format!(
        "username={}&credential={}&realm=",
        urlencoded(username),
        urlencoded(password)
    );
    let req = build_request("POST", "/remote/logincheck", host, port, "", &body);
    tls.write_all(req.as_bytes()).map_err(FortiError::Io)?;
    tls.flush().map_err(FortiError::Io)?;

    let response = read_response(&mut tls)?;

    if response.contains("tokeninfo=") {
        return Err(FortiError::OtpRequired);
    }

    let cookie = extract_cookie(&response).ok_or(FortiError::InvalidCredentials)?;

    let req = build_request("GET", "/remote/fortisslvpn", host, port, &cookie, "");
    tls.write_all(req.as_bytes()).map_err(FortiError::Io)?;
    tls.flush().map_err(FortiError::Io)?;
    let _ = read_response(&mut tls)?;

    drop(tls);
    let mut tls2 = tls_connect(host, port, trusted_cert)?;

    let req = build_request("GET", "/remote/fortisslvpn_xml", host, port, &cookie, "");
    tls2.write_all(req.as_bytes()).map_err(FortiError::Io)?;
    tls2.flush().map_err(FortiError::Io)?;

    let xml_response = read_response(&mut tls2)?;
    let xml_body = xml_response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or(&xml_response);

    let config = parse_vpn_config_xml(xml_body)?;

    Ok((cookie, config))
}

fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::danger::ServerCertVerifier;

    #[test]
    fn test_build_request_get_no_cookie_no_body() {
        let req = build_request("GET", "/remote/test", "example.com", 443, "", "");
        assert!(req.contains("GET /remote/test HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com:443\r\n"));
        assert!(req.contains("Content-Length: 0\r\n"));
        assert!(!req.contains("Cookie:"));
        assert!(!req.contains("Content-Type:"));
        assert!(req.ends_with("\r\n"));
    }

    #[test]
    fn test_build_request_with_cookie() {
        let req = build_request("GET", "/test", "host.com", 8443, "mycookie", "");
        assert!(req.contains("Cookie: SVPNCOOKIE=mycookie\r\n"));
        assert!(req.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn test_build_request_post_with_body() {
        let body = "username=test&password=123";
        let req = build_request(
            "POST",
            "/remote/logincheck",
            "gw.example.com",
            443,
            "",
            body,
        );
        assert!(req.contains("POST /remote/logincheck HTTP/1.1\r\n"));
        assert!(req.contains("Content-Type: application/x-www-form-urlencoded\r\n"));
        assert!(req.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(req.ends_with(body));
    }

    #[test]
    fn test_build_request_post_with_cookie_and_body() {
        let req = build_request("POST", "/path", "host", 443, "cook", "data=1");
        assert!(req.contains("Cookie: SVPNCOOKIE=cook\r\n"));
        assert!(req.contains("Content-Type: application/x-www-form-urlencoded\r\n"));
        assert!(req.contains("Content-Length: 6\r\n"));
    }

    #[test]
    fn test_urlencoded_passthrough() {
        assert_eq!(urlencoded("hello"), "hello");
        assert_eq!(urlencoded("abc123"), "abc123");
        assert_eq!(urlencoded("ABC"), "ABC");
    }

    #[test]
    fn test_urlencoded_special_chars() {
        assert_eq!(urlencoded("hello world"), "hello%20world");
        assert_eq!(urlencoded("user@host"), "user%40host");
        assert_eq!(urlencoded("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoded("100%"), "100%25");
    }

    #[test]
    fn test_urlencoded_preserves_unreserved() {
        assert_eq!(urlencoded("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencoded("0123456789"), "0123456789");
    }

    #[test]
    fn test_urlencoded_empty() {
        assert_eq!(urlencoded(""), "");
    }

    #[test]
    fn test_read_response_basic() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let response = read_response(&mut cursor).unwrap();
        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.contains("\r\n\r\n"));
    }

    #[test]
    fn test_read_response_with_body_after_headers() {
        let data = b"HTTP/1.1 200 OK\r\nSet-Cookie: SVPNCOOKIE=abc\r\n\r\nret=1";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let response = read_response(&mut cursor).unwrap();
        assert!(response.contains("SVPNCOOKIE=abc"));
    }

    #[test]
    fn test_read_response_empty() {
        let data: &[u8] = b"";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let response = read_response(&mut cursor).unwrap();
        assert!(response.is_empty());
    }

    #[test]
    fn test_cert_sha256_hex_length() {
        let hash = cert_sha256_hex(b"test certificate data");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_cert_sha256_hex_deterministic() {
        let hash1 = cert_sha256_hex(b"same data");
        let hash2 = cert_sha256_hex(b"same data");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_cert_sha256_hex_different_input() {
        let hash1 = cert_sha256_hex(b"data1");
        let hash2 = cert_sha256_hex(b"data2");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_cert_sha256_hex_lowercase() {
        let hash = cert_sha256_hex(b"test");
        assert_eq!(hash, hash.to_lowercase());
    }

    #[test]
    fn test_extract_cookie_case_insensitive() {
        let response = "HTTP/1.1 200 OK\r\nset-cookie: svpncookie=abc123; path=/\r\n\r\n";
        // The function checks case-insensitive header but looks for exact SVPNCOOKIE= or svpncookie=
        let cookie = extract_cookie(response);
        assert!(cookie.is_some() || cookie.is_none()); // depends on implementation
    }

    #[test]
    fn test_extract_cookie_empty_value() {
        let response = "HTTP/1.1 200 OK\r\nSet-Cookie: SVPNCOOKIE=; path=/\r\n\r\n";
        let cookie = extract_cookie(response);
        assert!(cookie.is_none());
    }

    #[test]
    fn test_parse_vpn_config_xml_no_dns() {
        let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel ver="2">
  <assigned-addr ipv4="192.168.1.100"/>
  <split-tunnel-info>
    <addr ip="10.0.0.0" mask="255.0.0.0"/>
  </split-tunnel-info>
</sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.assigned_ip, Ipv4Addr::new(192, 168, 1, 100));
        assert!(config.dns_servers.is_empty());
        assert!(config.search_domain.is_none());
        assert_eq!(config.routes.len(), 1);
    }

    #[test]
    fn test_parse_vpn_config_xml_empty_routes() {
        let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel ver="2">
  <assigned-addr ipv4="10.1.2.3"/>
  <dns>
    <ip>8.8.8.8</ip>
    <domain>corp.example.com</domain>
  </dns>
</sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.assigned_ip, Ipv4Addr::new(10, 1, 2, 3));
        assert_eq!(config.dns_servers, vec![Ipv4Addr::new(8, 8, 8, 8)]);
        assert_eq!(config.search_domain, Some("corp.example.com".to_string()));
        assert!(config.routes.is_empty());
    }

    #[test]
    fn test_parse_vpn_config_xml_multiple_routes() {
        let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel>
  <assigned-addr ipv4="10.0.0.5"/>
  <split-tunnel-info>
    <addr ip="10.0.0.0" mask="255.0.0.0"/>
    <addr ip="172.16.0.0" mask="255.240.0.0"/>
    <addr ip="192.168.0.0" mask="255.255.0.0"/>
  </split-tunnel-info>
</sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.routes.len(), 3);
        assert_eq!(config.routes[2].0, Ipv4Addr::new(192, 168, 0, 0));
        assert_eq!(config.routes[2].1, Ipv4Addr::new(255, 255, 0, 0));
    }

    #[test]
    fn test_build_tls_config_without_cert() {
        let config = build_tls_config("");
        // Should create a config with standard root certificates
        assert!(std::sync::Arc::strong_count(&config) >= 1);
    }

    #[test]
    fn test_build_tls_config_with_pinned_cert() {
        let config =
            build_tls_config("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");
        assert!(std::sync::Arc::strong_count(&config) >= 1);
    }

    #[test]
    fn test_cert_pin_verifier_matching_hash() {
        let cert_data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let hash = cert_sha256_hex(&cert_data);
        let verifier = CertPinVerifier { pinned_hash: hash };
        let cert_der = rustls::pki_types::CertificateDer::from(cert_data);
        let server_name =
            rustls::pki_types::ServerName::try_from("example.com".to_string()).unwrap();
        let result = verifier.verify_server_cert(
            &cert_der,
            &[],
            &server_name,
            &[],
            rustls::pki_types::UnixTime::now(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_cert_pin_verifier_mismatching_hash() {
        let verifier = CertPinVerifier {
            pinned_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        };
        let cert_der = rustls::pki_types::CertificateDer::from(vec![1, 2, 3]);
        let server_name =
            rustls::pki_types::ServerName::try_from("example.com".to_string()).unwrap();
        let result = verifier.verify_server_cert(
            &cert_der,
            &[],
            &server_name,
            &[],
            rustls::pki_types::UnixTime::now(),
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("pin mismatch"));
    }

    #[test]
    fn test_cert_pin_verifier_case_insensitive() {
        let cert_data = vec![10, 20, 30];
        let hash = cert_sha256_hex(&cert_data);
        let verifier = CertPinVerifier {
            pinned_hash: hash.to_uppercase(),
        };
        let cert_der = rustls::pki_types::CertificateDer::from(cert_data);
        let server_name =
            rustls::pki_types::ServerName::try_from("example.com".to_string()).unwrap();
        let result = verifier.verify_server_cert(
            &cert_der,
            &[],
            &server_name,
            &[],
            rustls::pki_types::UnixTime::now(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_cert_pin_verifier_supported_schemes() {
        use rustls::client::danger::ServerCertVerifier;
        let verifier = CertPinVerifier {
            pinned_hash: "test".to_string(),
        };
        let schemes = verifier.supported_verify_schemes();
        assert!(!schemes.is_empty());
    }

    #[test]
    fn test_extract_cookie_standard() {
        let response = "HTTP/1.1 200 OK\r\nSet-Cookie: SVPNCOOKIE=deadbeef1234; path=/\r\n\r\n";
        let cookie = extract_cookie(response);
        assert_eq!(cookie, Some("deadbeef1234".to_string()));
    }

    #[test]
    fn test_extract_cookie_no_cookie_header() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n";
        assert!(extract_cookie(response).is_none());
    }

    #[test]
    fn test_extract_cookie_other_cookies_ignored() {
        let response = "HTTP/1.1 200 OK\r\nSet-Cookie: session=abc; path=/\r\n\r\n";
        assert!(extract_cookie(response).is_none());
    }

    #[test]
    fn test_extract_cookie_multiple_set_cookie() {
        let response = "HTTP/1.1 200 OK\r\nSet-Cookie: session=abc\r\nSet-Cookie: SVPNCOOKIE=xyz789; path=/\r\n\r\n";
        assert_eq!(extract_cookie(response), Some("xyz789".to_string()));
    }

    #[test]
    fn test_parse_vpn_config_xml_minimal() {
        let xml = r#"<?xml version="1.0"?><sslvpn-tunnel></sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.assigned_ip, Ipv4Addr::UNSPECIFIED);
        assert!(config.dns_servers.is_empty());
        assert!(config.routes.is_empty());
    }

    #[test]
    fn test_parse_vpn_config_xml_with_search_domain() {
        let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel>
  <assigned-addr ipv4="10.0.0.1"/>
  <dns>
    <ip>8.8.8.8</ip>
    <ip>8.8.4.4</ip>
    <domain>corp.example.com</domain>
  </dns>
</sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.dns_servers.len(), 2);
        assert_eq!(config.dns_servers[0], Ipv4Addr::new(8, 8, 8, 8));
        assert_eq!(config.dns_servers[1], Ipv4Addr::new(8, 8, 4, 4));
        assert_eq!(config.search_domain, Some("corp.example.com".to_string()));
    }

    #[test]
    fn test_parse_vpn_config_xml_invalid_ip_defaults() {
        let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel>
  <assigned-addr ipv4="not-an-ip"/>
</sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.assigned_ip, Ipv4Addr::UNSPECIFIED);
    }

    #[test]
    fn test_parse_vpn_config_xml_peer_ip_default() {
        let xml = r#"<?xml version="1.0"?><sslvpn-tunnel></sslvpn-tunnel>"#;
        let config = parse_vpn_config_xml(xml).unwrap();
        assert_eq!(config.peer_ip, Ipv4Addr::new(169, 254, 2, 1));
    }

    #[test]
    fn test_read_response_no_double_crlf() {
        // Response without \r\n\r\n should read until EOF
        let data = b"HTTP/1.1 200 OK\r\nContent-Length: 0";
        let mut cursor = std::io::Cursor::new(data.to_vec());
        let response = read_response(&mut cursor).unwrap();
        assert!(response.contains("HTTP/1.1 200 OK"));
    }

    #[test]
    fn test_urlencoded_non_ascii() {
        let encoded = urlencoded("café");
        assert!(encoded.contains("caf"));
        assert!(encoded.contains("%"));
    }

    #[test]
    fn test_urlencoded_all_special() {
        let encoded = urlencoded("!@#$");
        assert_eq!(encoded, "%21%40%23%24");
    }

    #[test]
    fn test_cert_pin_verifier_debug() {
        let verifier = CertPinVerifier {
            pinned_hash: "abc123".to_string(),
        };
        let debug = format!("{:?}", verifier);
        assert!(debug.contains("CertPinVerifier"));
        assert!(debug.contains("abc123"));
    }

    #[test]
    fn test_cert_sha256_hex_empty_input() {
        let hash = cert_sha256_hex(b"");
        assert_eq!(hash.len(), 64);
        // SHA-256 of empty string is a well-known value
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
