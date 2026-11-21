//! Bounded, streaming body copies. A body never sits in memory: every direction reuses one
//! fixed-size buffer and flushes as it goes.

use std::io::{self, BufRead, Read, Write};

use super::RELAY_CAP;
use super::framing::Framing;

/// Largest chunk-size or trailer line.
const CHUNK_LINE_CAP: usize = 1024;

/// Largest number of trailer lines after the terminating zero chunk.
const MAX_TRAILER_LINES: usize = 64;

/// Copy one body according to its framing.
pub(crate) fn copy_body<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    framing: Framing,
) -> io::Result<()> {
    match framing {
        Framing::None => Ok(()),
        Framing::Length(length) => copy_length(reader, writer, length),
        Framing::Chunked => copy_chunked(reader, writer),
        Framing::UntilEof => pump_bytes(reader, writer),
    }
}

/// Copy exactly `remaining` bytes.
fn copy_length<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    mut remaining: u64,
) -> io::Result<()> {
    let mut buffer = [0_u8; RELAY_CAP];
    while remaining > 0 {
        let want = usize::try_from(remaining.min(RELAY_CAP as u64)).unwrap_or(RELAY_CAP);
        let read = reader.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "body ended before its Content-Length bytes arrived",
            ));
        }
        writer.write_all(&buffer[..read])?;
        writer.flush()?;
        remaining -= read as u64;
    }
    Ok(())
}

/// Copy `Transfer-Encoding: chunked` framing verbatim, buffered so no size line costs a syscall.
fn copy_chunked<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    loop {
        let line = read_line(reader)?;
        writer.write_all(&line)?;
        writer.flush()?;
        let size = chunk_size(&line)?;
        if size == 0 {
            for _ in 0..MAX_TRAILER_LINES {
                let trailer = read_line(reader)?;
                writer.write_all(&trailer)?;
                writer.flush()?;
                if trailer == b"\r\n" {
                    return Ok(());
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunked body carries too many trailer lines",
            ));
        }
        copy_length(reader, writer, size)?;
        let mut crlf = [0_u8; 2];
        reader.read_exact(&mut crlf)?;
        if crlf != *b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "chunk data is not terminated by CRLF",
            ));
        }
        writer.write_all(b"\r\n")?;
        writer.flush()?;
    }
}

/// Read one CRLF-terminated framing line through the reader's buffer.
fn read_line<R: BufRead>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(64);
    let read = reader.read_until(b'\n', &mut line)?;
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "chunked body ended before its framing line",
        ));
    }
    if line.len() > CHUNK_LINE_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunk framing line exceeds the cap",
        ));
    }
    if !line.ends_with(b"\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunk framing line is not CRLF-terminated",
        ));
    }
    Ok(line)
}

/// Parse a hex chunk size, ignoring chunk extensions.
fn chunk_size(line: &[u8]) -> io::Result<u64> {
    let text = std::str::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size is not ASCII"))?;
    let size = text
        .trim_end_matches(['\r', '\n'])
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    u64::from_str_radix(size, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size is not hexadecimal"))
}

/// Pump bytes one way until EOF, flushing after every chunk.
pub(crate) fn pump_bytes<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<()> {
    let mut buffer = [0_u8; RELAY_CAP];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(read) => {
                writer.write_all(&buffer[..read])?;
                writer.flush()?;
            }
        }
    }
}
