//! `Content-Length` framing for LSP over stdio, with hard header and body bounds.
//!
//! Nothing in this module echoes a document or a secret: every error variant carries only a
//! generic reason, so a malformed frame can never leak editor text to the terminal.

use std::fmt;
use std::io::{self, BufRead, Write};

/// Maximum bytes of request headers (the spec allows more; editors send far less).
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Maximum bytes of one JSON-RPC body (a whole `Forkfile` plus hover text).
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Why one framed message could not be read.
#[derive(Debug)]
pub enum FramingError {
    /// The header block exceeded [`MAX_HEADER_BYTES`].
    HeaderTooLarge,
    /// `Content-Length` exceeded [`MAX_BODY_BYTES`].
    BodyTooLarge,
    /// No `Content-Length` header was present.
    MissingContentLength,
    /// `Content-Length` (or another header line) was not valid ASCII/number.
    InvalidHeader,
    /// The transport failed.
    Io(io::Error),
}

impl fmt::Display for FramingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderTooLarge => formatter.write_str("LSP header block is too large"),
            Self::BodyTooLarge => formatter.write_str("LSP body is too large"),
            Self::MissingContentLength => formatter.write_str("LSP frame has no Content-Length"),
            Self::InvalidHeader => formatter.write_str("LSP frame has an invalid header"),
            Self::Io(error) => write!(formatter, "LSP transport error: {error}"),
        }
    }
}

impl std::error::Error for FramingError {}

/// Read one framed message. `Ok(None)` means a clean end of input.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, FramingError> {
    let Some(header) = read_header_block(reader)? else {
        return Ok(None);
    };
    let mut content_length = None;
    for raw in header.split(|byte| *byte == b'\n') {
        let line = std::str::from_utf8(raw).map_err(|_| FramingError::InvalidHeader)?;
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(FramingError::InvalidHeader);
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| FramingError::InvalidHeader)?,
            );
        }
    }
    let length = content_length.ok_or(FramingError::MissingContentLength)?;
    if length > MAX_BODY_BYTES {
        return Err(FramingError::BodyTooLarge);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(FramingError::Io)?;
    Ok(Some(body))
}

fn read_header_block(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, FramingError> {
    let mut header = Vec::new();
    let mut line = Vec::new();
    loop {
        let Some(byte) = read_byte(reader).map_err(FramingError::Io)? else {
            return Ok(None);
        };
        if byte == b'\n' {
            if line.is_empty() || line == b"\r" {
                break;
            }
            header.extend_from_slice(&line);
            header.push(b'\n');
            line.clear();
            if header.len() > MAX_HEADER_BYTES {
                return Err(FramingError::HeaderTooLarge);
            }
        } else {
            line.push(byte);
            if header.len() + line.len() > MAX_HEADER_BYTES {
                return Err(FramingError::HeaderTooLarge);
            }
        }
    }
    Ok(Some(header))
}

fn read_byte(reader: &mut impl BufRead) -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Write one framed message and flush it.
///
/// Header and body are assembled into a single buffer and written once, so a caller can never
/// observe (or recover after) a half-frame.
pub fn write_message(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn round_trips_one_message() {
        let mut encoded = Vec::new();
        write_message(&mut encoded, br#"{"jsonrpc":"2.0"}"#).expect("write");
        let mut cursor = Cursor::new(encoded);
        assert_eq!(
            read_message(&mut cursor).expect("read").expect("message"),
            br#"{"jsonrpc":"2.0"}"#
        );
        assert!(read_message(&mut cursor).expect("eof").is_none());
    }

    #[test]
    fn rejects_oversized_headers_and_bodies() {
        let mut header = Cursor::new(vec![b'a'; MAX_HEADER_BYTES + 4]);
        assert!(matches!(
            read_message(&mut header),
            Err(FramingError::HeaderTooLarge)
        ));

        let body = format!("Content-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1);
        let mut oversized = Cursor::new(body.into_bytes());
        assert!(matches!(
            read_message(&mut oversized),
            Err(FramingError::BodyTooLarge)
        ));

        let mut missing = Cursor::new(b"Content-Type: application/json\r\n\r\n".to_vec());
        assert!(matches!(
            read_message(&mut missing),
            Err(FramingError::MissingContentLength)
        ));
    }
}
