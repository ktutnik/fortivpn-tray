use std::time::Duration;

use fortivpn::tunnel::{
    decode_frame_header, encode_frame, read_frame, write_frame, FrameReader, HEADER_SIZE, MAGIC,
};
use tokio::io::AsyncWriteExt;

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

// === FrameReader (cancel-safe) tests ===

#[tokio::test]
async fn test_frame_reader_reads_sequential_frames() {
    let mut buf = Vec::new();
    write_frame(&mut buf, b"first").await.unwrap();
    write_frame(&mut buf, b"second").await.unwrap();
    write_frame(&mut buf, b"").await.unwrap();

    let mut frames = FrameReader::new(&buf[..]);
    assert_eq!(frames.next_frame().await.unwrap(), b"first");
    assert_eq!(frames.next_frame().await.unwrap(), b"second");
    assert!(frames.next_frame().await.unwrap().is_empty());
}

#[tokio::test]
async fn test_frame_reader_resumes_after_cancellation_mid_frame() {
    // The bug this guards: the reader future is dropped after the 6-byte header
    // has been consumed but before the payload arrives. A non-cancel-safe reader
    // loses those header bytes and reads payload as a header on the next call —
    // magic check fails, tunnel torn down.
    let (mut client, server) = tokio::io::duplex(64);
    let mut frames = FrameReader::new(server);

    let frame = encode_frame(b"payload");
    client.write_all(&frame[..HEADER_SIZE]).await.unwrap();

    // Header is available, payload is not — this call must time out mid-frame.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), frames.next_frame())
            .await
            .is_err(),
        "expected the read to still be waiting for the payload"
    );

    client.write_all(&frame[HEADER_SIZE..]).await.unwrap();
    assert_eq!(frames.next_frame().await.unwrap(), b"payload");
}

#[tokio::test]
async fn test_frame_reader_resumes_after_cancellation_mid_header() {
    let (mut client, server) = tokio::io::duplex(64);
    let mut frames = FrameReader::new(server);

    let frame = encode_frame(b"hi");
    client.write_all(&frame[..3]).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), frames.next_frame())
            .await
            .is_err()
    );

    client.write_all(&frame[3..]).await.unwrap();
    assert_eq!(frames.next_frame().await.unwrap(), b"hi");
}

#[tokio::test]
async fn test_frame_reader_survives_repeated_cancellation() {
    // One byte at a time, cancelled between every single byte.
    let (mut client, server) = tokio::io::duplex(64);
    let mut frames = FrameReader::new(server);

    let frame = encode_frame(b"abcd");
    for byte in &frame[..frame.len() - 1] {
        client.write_all(&[*byte]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), frames.next_frame())
                .await
                .is_err()
        );
    }
    client.write_all(&frame[frame.len() - 1..]).await.unwrap();
    assert_eq!(frames.next_frame().await.unwrap(), b"abcd");
}

#[tokio::test]
async fn test_frame_reader_bad_magic_errors() {
    let bad = [0u8, 5, 0xAA, 0xAA, 0, 0];
    let mut frames = FrameReader::new(&bad[..]);
    assert!(frames.next_frame().await.is_err());
}

#[tokio::test]
async fn test_frame_reader_closed_tunnel_errors() {
    let (client, server) = tokio::io::duplex(64);
    drop(client);
    let mut frames = FrameReader::new(server);
    assert!(frames.next_frame().await.is_err());
}

#[tokio::test]
async fn test_frame_reader_handles_large_payload_split_across_reads() {
    let payload = vec![0x5Au8; 1500];
    let frame = encode_frame(&payload);
    let (mut client, server) = tokio::io::duplex(64);

    let writer = tokio::spawn(async move {
        for chunk in frame.chunks(37) {
            client.write_all(chunk).await.unwrap();
        }
    });

    let mut frames = FrameReader::new(server);
    assert_eq!(frames.next_frame().await.unwrap(), payload);
    writer.await.unwrap();
}
