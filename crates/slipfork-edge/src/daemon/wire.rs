//! Bounded JSON-line framing shared by the control server and client.
//!
//! A frame is one `\n`-terminated JSON object of at most [`MAX_CONTROL_FRAME`] bytes. The reader is
//! buffered across calls, so a single OS read carrying several frames (or a frame plus the next
//! one) never drops bytes. Oversized or truncated frames fail closed with
//! [`EdgeError::ControlFrame`] / [`EdgeError::ControlProtocol`]; nothing is truncated and retried.

use std::io::{self, Read, Write};

use slipfork_engine::{LogChunk, StreamChunk};

use crate::EdgeError;
use crate::daemon::{DaemonReply, MAX_CONTROL_FRAME, MAX_STREAM_CHUNK};

/// Encode and write one `\n`-terminated JSON frame, refusing anything above the frame cap.
pub(crate) fn write_reply(writer: &mut impl Write, reply: &DaemonReply) -> Result<(), EdgeError> {
    let mut bytes = serde_json::to_vec(reply).map_err(|err| EdgeError::ControlFrame {
        reason: err.to_string(),
    })?;
    if bytes.len() > MAX_CONTROL_FRAME {
        return Err(EdgeError::ControlFrame {
            reason: format!(
                "reply frame is {} bytes, above the {MAX_CONTROL_FRAME} byte cap",
                bytes.len()
            ),
        });
    }
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .map_err(|err| EdgeError::ControlIo {
            operation: "write reply",
            message: err.to_string(),
        })
}

/// Split one stream chunk into frame-sized pieces and write each as a `chunk` reply.
pub(crate) fn write_chunk(writer: &mut impl Write, chunk: StreamChunk) -> Result<(), EdgeError> {
    if chunk.bytes.len() <= MAX_STREAM_CHUNK {
        return write_reply(writer, &DaemonReply::Chunk { chunk });
    }
    let source = chunk.source;
    for piece in chunk.bytes.chunks(MAX_STREAM_CHUNK) {
        write_reply(
            writer,
            &DaemonReply::Chunk {
                chunk: StreamChunk {
                    source,
                    bytes: piece.to_vec(),
                },
            },
        )?;
    }
    Ok(())
}

/// Split one attributed log chunk into frame-sized pieces and write each as a `log` reply.
///
/// The component/hook id and the stream are copied onto every piece, so a chunk larger than one
/// frame stays attributable after the split; no piece is ever sent without its label.
pub(crate) fn write_log(writer: &mut impl Write, log: LogChunk) -> Result<(), EdgeError> {
    if log.bytes.len() <= MAX_STREAM_CHUNK {
        return write_reply(writer, &DaemonReply::Log { log });
    }
    for piece in log.bytes.chunks(MAX_STREAM_CHUNK) {
        write_reply(
            writer,
            &DaemonReply::Log {
                log: LogChunk {
                    target: log.target.clone(),
                    source: log.source,
                    bytes: piece.to_vec(),
                },
            },
        )?;
    }
    Ok(())
}

/// A buffered, bounded JSON-line frame reader.
pub(crate) struct FrameReader<R> {
    reader: R,
    pending: Vec<u8>,
}

impl<R: Read> FrameReader<R> {
    /// Wrap a reader; bytes already buffered are preserved across every [`Self::read_frame`].
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    /// Read one `\n`-terminated frame; `Ok(None)` only when the peer closed between frames.
    pub(crate) fn read_frame(&mut self) -> Result<Option<Vec<u8>>, EdgeError> {
        let mut chunk = [0_u8; 4096];
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                if end > MAX_CONTROL_FRAME {
                    return Err(frame_too_large());
                }
                let mut frame: Vec<u8> = self.pending.drain(..=end).collect();
                frame.pop();
                return Ok(Some(frame));
            }
            if self.pending.len() > MAX_CONTROL_FRAME {
                return Err(frame_too_large());
            }
            let read = self
                .reader
                .read(&mut chunk)
                .map_err(|err| EdgeError::ControlIo {
                    operation: "read frame",
                    message: err.to_string(),
                })?;
            if read == 0 {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err(EdgeError::ControlProtocol {
                        reason: "peer closed in the middle of a control frame".to_owned(),
                    })
                };
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    /// Block until the peer closes the connection (the `forward` lifetime contract).
    pub(crate) fn drain_until_close(&mut self) -> io::Result<()> {
        self.pending.clear();
        let mut sink = [0_u8; 256];
        loop {
            match self.reader.read(&mut sink) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }
}

fn frame_too_large() -> EdgeError {
    EdgeError::ControlFrame {
        reason: format!("frame exceeds the {MAX_CONTROL_FRAME} byte cap"),
    }
}

/// The reply a connection received does not answer the request it sent.
pub(crate) fn unexpected_reply(reply: &DaemonReply) -> EdgeError {
    EdgeError::ControlProtocol {
        reason: format!("unexpected reply {reply:?}"),
    }
}
