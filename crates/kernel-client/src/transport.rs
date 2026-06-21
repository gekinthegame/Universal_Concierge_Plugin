//! Length-prefixed framing over a byte stream: a 4-byte big-endian length, then
//! that many bytes of JSON payload. The framing is transport-agnostic — Phase 1
//! runs it over a Unix domain socket; a Windows named pipe (Phase 6) reuses this
//! unchanged, since both are just `Read + Write` byte streams.

use std::io::{self, Read, Write};

/// Large enough for the current JSON endpoints, small enough to prevent a local
/// client from forcing an unbounded allocation through a bogus length prefix.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Write one frame (4-byte big-endian length + payload) and flush.
pub fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

/// Read one frame. `Ok(None)` means the peer closed cleanly between frames (not an
/// error — just the end of the conversation).
pub fn read_frame(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::{read_frame, write_frame, MAX_FRAME_BYTES};
    use std::io::{Cursor, ErrorKind};

    #[test]
    fn frame_round_trips() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, br#"{"ok":true}"#).unwrap();
        let mut cursor = Cursor::new(bytes);
        let frame = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(frame, br#"{"ok":true}"#);
    }

    #[test]
    fn read_rejects_oversized_frame_before_allocating() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes());
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn write_rejects_oversized_frame() {
        let payload = vec![0u8; MAX_FRAME_BYTES + 1];
        let err = write_frame(&mut Vec::new(), &payload).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}
