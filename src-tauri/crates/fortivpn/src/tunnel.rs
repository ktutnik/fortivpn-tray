use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAGIC: u16 = 0x5050;
pub const HEADER_SIZE: usize = 6;

pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let total_length = (HEADER_SIZE + payload.len()) as u16;
    let payload_size = payload.len() as u16;
    let mut frame = Vec::with_capacity(HEADER_SIZE + payload.len());
    frame.extend_from_slice(&total_length.to_be_bytes());
    frame.extend_from_slice(&MAGIC.to_be_bytes());
    frame.extend_from_slice(&payload_size.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub fn decode_frame_header(header: &[u8; HEADER_SIZE]) -> Result<usize, String> {
    let magic = u16::from_be_bytes([header[2], header[3]]);
    if magic != MAGIC {
        if &header[..5] == b"HTTP/" {
            return Err("Tunnel rejected: gateway returned HTTP error".to_string());
        }
        return Err(format!("Invalid magic: expected 0x{MAGIC:04X}, got 0x{magic:04X}"));
    }
    let payload_size = u16::from_be_bytes([header[4], header[5]]) as usize;
    Ok(payload_size)
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut header = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header).await.map_err(|e| format!("Read header: {e}"))?;
    let payload_size = decode_frame_header(&header)?;
    let mut payload = vec![0u8; payload_size];
    if payload_size > 0 {
        reader.read_exact(&mut payload).await.map_err(|e| format!("Read payload: {e}"))?;
    }
    Ok(payload)
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, payload: &[u8]) -> Result<(), String> {
    let frame = encode_frame(payload);
    writer.write_all(&frame).await.map_err(|e| format!("Write frame: {e}"))?;
    writer.flush().await.map_err(|e| format!("Flush: {e}"))?;
    Ok(())
}
