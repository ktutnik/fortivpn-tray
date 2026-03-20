use fortivpn::tunnel::{
    decode_frame_header, encode_frame, read_frame, write_frame, HEADER_SIZE, MAGIC,
};

// === encode_frame tests ===

#[test]
fn test_encode_frame() {
    let payload = b"hello";
    let frame = encode_frame(payload);
    assert_eq!(frame.len(), 6 + 5);
    assert_eq!(&frame[0..2], &11u16.to_be_bytes());
    assert_eq!(&frame[2..4], &MAGIC.to_be_bytes());
    assert_eq!(&frame[4..6], &5u16.to_be_bytes());
    assert_eq!(&frame[6..], b"hello");
}

#[test]
fn test_encode_empty_payload() {
    let frame = encode_frame(b"");
    assert_eq!(frame.len(), 6);
    assert_eq!(&frame[0..2], &6u16.to_be_bytes());
    assert_eq!(&frame[2..4], &MAGIC.to_be_bytes());
    assert_eq!(&frame[4..6], &0u16.to_be_bytes());
}

#[test]
fn test_encode_large_payload() {
    let payload = vec![0xAA; 1000];
    let frame = encode_frame(&payload);
    assert_eq!(frame.len(), 6 + 1000);
    assert_eq!(&frame[0..2], &1006u16.to_be_bytes());
    assert_eq!(&frame[6..], payload.as_slice());
}

// === decode_frame_header tests ===

#[test]
fn test_decode_frame_header() {
    let header = [0x00, 0x0B, 0x50, 0x50, 0x00, 0x05];
    let payload_size = decode_frame_header(&header).unwrap();
    assert_eq!(payload_size, 5);
}

#[test]
fn test_decode_frame_header_bad_magic() {
    let header = [0x00, 0x0B, 0x48, 0x54, 0x00, 0x05];
    assert!(decode_frame_header(&header).is_err());
}

#[test]
fn test_decode_frame_header_http_error() {
    // "HTTP/" as first 5 bytes triggers specific error
    let header = [b'H', b'T', b'T', b'P', b'/', b' '];
    let err = decode_frame_header(&header).unwrap_err();
    assert!(err.contains("HTTP error") || err.contains("HTTP"));
}

#[test]
fn test_decode_frame_header_zero_payload() {
    let header = [0x00, 0x06, 0x50, 0x50, 0x00, 0x00];
    let payload_size = decode_frame_header(&header).unwrap();
    assert_eq!(payload_size, 0);
}

#[test]
fn test_decode_frame_header_large_payload() {
    let header = [0x03, 0xEE, 0x50, 0x50, 0x03, 0xE8]; // 1000 payload
    let payload_size = decode_frame_header(&header).unwrap();
    assert_eq!(payload_size, 1000);
}

// === Constants ===

#[test]
fn test_constants() {
    assert_eq!(MAGIC, 0x5050);
    assert_eq!(HEADER_SIZE, 6);
}

// === Async read_frame / write_frame tests ===

#[tokio::test]
async fn test_read_frame_basic() {
    let payload = b"test data";
    let frame = encode_frame(payload);
    let mut reader: &[u8] = &frame;
    let result = read_frame(&mut reader).await.unwrap();
    assert_eq!(result, b"test data");
}

#[tokio::test]
async fn test_read_frame_empty_payload() {
    let frame = encode_frame(b"");
    let mut reader: &[u8] = &frame;
    let result = read_frame(&mut reader).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_read_frame_large_payload() {
    let payload = vec![0xBB; 500];
    let frame = encode_frame(&payload);
    let mut reader: &[u8] = &frame;
    let result = read_frame(&mut reader).await.unwrap();
    assert_eq!(result, payload);
}

#[tokio::test]
async fn test_read_frame_multiple_frames() {
    let frame1 = encode_frame(b"first");
    let frame2 = encode_frame(b"second");
    let mut data = Vec::new();
    data.extend(&frame1);
    data.extend(&frame2);
    let mut reader: &[u8] = &data;

    let r1 = read_frame(&mut reader).await.unwrap();
    assert_eq!(r1, b"first");
    let r2 = read_frame(&mut reader).await.unwrap();
    assert_eq!(r2, b"second");
}

#[tokio::test]
async fn test_read_frame_truncated_header() {
    let data = vec![0x00, 0x0B, 0x50]; // only 3 bytes, need 6
    let mut reader: &[u8] = &data;
    let result = read_frame(&mut reader).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_read_frame_bad_magic() {
    let data = vec![0x00, 0x0B, 0xAA, 0xBB, 0x00, 0x05, 1, 2, 3, 4, 5];
    let mut reader: &[u8] = &data;
    let result = read_frame(&mut reader).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_write_frame_basic() {
    let mut buf = Vec::new();
    write_frame(&mut buf, b"hello").await.unwrap();
    // Verify the written data is a valid frame
    let mut reader: &[u8] = &buf;
    let result = read_frame(&mut reader).await.unwrap();
    assert_eq!(result, b"hello");
}

#[tokio::test]
async fn test_write_frame_empty() {
    let mut buf = Vec::new();
    write_frame(&mut buf, b"").await.unwrap();
    let mut reader: &[u8] = &buf;
    let result = read_frame(&mut reader).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_write_read_roundtrip() {
    let payloads: Vec<Vec<u8>> = vec![b"first".to_vec(), b"second".to_vec(), vec![0xFF; 100]];
    let mut buf = Vec::new();
    for p in &payloads {
        write_frame(&mut buf, p).await.unwrap();
    }
    let mut reader: &[u8] = &buf;
    for p in &payloads {
        let result = read_frame(&mut reader).await.unwrap();
        assert_eq!(&result, p);
    }
}
