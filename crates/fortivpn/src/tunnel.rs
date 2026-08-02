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

/// Append an encoded frame to `buf`.
///
/// The allocating [`encode_frame`] costs a `Vec` per packet; this reuses one
/// buffer so a burst of packets becomes a single allocation and a single write.
pub fn encode_frame_into(buf: &mut Vec<u8>, payload: &[u8]) {
    let total_length = (HEADER_SIZE + payload.len()) as u16;
    let payload_size = payload.len() as u16;
    buf.extend_from_slice(&total_length.to_be_bytes());
    buf.extend_from_slice(&MAGIC.to_be_bytes());
    buf.extend_from_slice(&payload_size.to_be_bytes());
    buf.extend_from_slice(payload);
}

pub fn decode_frame_header(header: &[u8; HEADER_SIZE]) -> Result<usize, String> {
    let magic = u16::from_be_bytes([header[2], header[3]]);
    if magic != MAGIC {
        if &header[..5] == b"HTTP/" {
            return Err("Tunnel rejected: gateway returned HTTP error".to_string());
        }
        return Err(format!(
            "Invalid magic: expected 0x{MAGIC:04X}, got 0x{magic:04X}"
        ));
    }
    let payload_size = u16::from_be_bytes([header[4], header[5]]) as usize;
    Ok(payload_size)
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut header = [0u8; HEADER_SIZE];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("Read header: {e}"))?;
    let payload_size = decode_frame_header(&header)?;
    let mut payload = vec![0u8; payload_size];
    if payload_size > 0 {
        reader
            .read_exact(&mut payload)
            .await
            .map_err(|e| format!("Read payload: {e}"))?;
    }
    Ok(payload)
}

/// Cancel-safe reader for 0x5050 frames.
///
/// [`read_frame`] is **not** cancel-safe: it awaits the 6-byte header, then the
/// payload. Dropping that future between the two awaits — which is exactly what
/// `tokio::select!` does when another branch wins — leaves the header bytes
/// consumed from the socket but nowhere recorded. The next read then parses
/// payload bytes as a header, the magic check fails, and a healthy tunnel is
/// torn down.
///
/// This reader keeps every partial read in the struct, so a dropped
/// [`FrameReader::next_frame`] future loses nothing: the next call resumes where
/// the last one stopped.
pub struct FrameReader<R> {
    reader: R,
    header: [u8; HEADER_SIZE],
    header_filled: usize,
    payload: Vec<u8>,
    payload_filled: usize,
    /// `Some` once the header is complete — the payload length it declared.
    payload_size: Option<usize>,
}

impl<R: AsyncReadExt + Unpin> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            header: [0u8; HEADER_SIZE],
            header_filled: 0,
            payload: Vec::new(),
            payload_filled: 0,
            payload_size: None,
        }
    }

    /// Read the next complete frame.
    ///
    /// Cancel-safe: every byte read is recorded in `self` before the next await,
    /// so dropping this future mid-frame only postpones the frame.
    pub async fn next_frame(&mut self) -> Result<Vec<u8>, String> {
        loop {
            match self.payload_size {
                None => {
                    let n = self
                        .reader
                        .read(&mut self.header[self.header_filled..])
                        .await
                        .map_err(|e| format!("Read header: {e}"))?;
                    if n == 0 {
                        return Err("Read header: tunnel closed".to_string());
                    }
                    self.header_filled += n;
                    if self.header_filled == HEADER_SIZE {
                        let size = decode_frame_header(&self.header)?;
                        self.payload = vec![0u8; size];
                        self.payload_filled = 0;
                        self.payload_size = Some(size);
                    }
                }
                Some(size) => {
                    if self.payload_filled == size {
                        let frame = std::mem::take(&mut self.payload);
                        self.header_filled = 0;
                        self.payload_filled = 0;
                        self.payload_size = None;
                        return Ok(frame);
                    }
                    let n = self
                        .reader
                        .read(&mut self.payload[self.payload_filled..])
                        .await
                        .map_err(|e| format!("Read payload: {e}"))?;
                    if n == 0 {
                        return Err("Read payload: tunnel closed".to_string());
                    }
                    self.payload_filled += n;
                }
            }
        }
    }
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), String> {
    let frame = encode_frame(payload);
    writer
        .write_all(&frame)
        .await
        .map_err(|e| format!("Write frame: {e}"))?;
    writer.flush().await.map_err(|e| format!("Flush: {e}"))?;
    Ok(())
}
