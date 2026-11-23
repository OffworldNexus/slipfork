//! Frame reading and writing for `<runtime_dir>/daemon.sock`.
//!
//! The first `recvmsg` is never buffered: `ingress.accept` carries its accepted connection
//! descriptor there, and a `BufReader` would discard the ancillary data.

use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamInput};

use super::wire;

/// Reads client `input` frames and forwards them to the world's exec stdin.
pub(super) fn pump_input(reader: &mut FrameReader, mut input: StreamInput) {
    loop {
        let Ok(Some((payload, _))) = reader.next() else {
            break;
        };
        let Ok(frame) = serde_json::from_slice::<wire::InputFrame>(&payload) else {
            continue;
        };
        let eof = frame.input.as_deref().is_none_or(str::is_empty);
        let data = frame.input.unwrap_or_default();
        if input.send_base64(&data).is_err() || eof {
            break;
        }
    }
}

/// Forwards client `resize` frames to the world PTY (one `ExecResize` per resize).
pub(super) fn pump_resizes(reader: &mut FrameReader, mut world: UnixStream) {
    loop {
        let Ok(Some((payload, _))) = reader.next() else {
            return;
        };
        let Ok(frame) = serde_json::from_slice::<wire::ResizeFrame>(&payload) else {
            continue;
        };
        let Some(resize) = frame.resize else {
            continue;
        };
        let request = Request::ExecResize {
            v: PROTOCOL_VERSION,
            id: "daemon-pty".to_owned(),
            cols: resize.cols,
            rows: resize.rows,
        };
        let Ok(bytes) = slipfork_world::control::encode(&request) else {
            continue;
        };
        if world
            .write_all(&bytes)
            .and_then(|()| world.flush())
            .is_err()
        {
            return;
        }
    }
}

/// One request reader that keeps the first descriptor, then reads plain NDJSON lines.
pub(super) struct FrameReader {
    stream: UnixStream,
    buffer: Vec<u8>,
    first: bool,
}

impl FrameReader {
    pub(super) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            first: true,
        }
    }

    pub(super) fn next(&mut self) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
        let mut descriptor = None;
        if self.first {
            self.first = false;
            let (payload, received) = slipfork_world::fdpass::recv_fd(&self.stream)?;
            self.buffer.extend_from_slice(&payload);
            descriptor = received;
        }
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let rest = self.buffer.split_off(index + 1);
                let mut line = std::mem::replace(&mut self.buffer, rest);
                line.truncate(index);
                return Ok(Some((line, descriptor)));
            }
            if self.buffer.len() > 1 << 20 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control frame exceeds 1 MiB",
                ));
            }
            let mut chunk = [0_u8; 4096];
            let read = self.stream.read(&mut chunk)?;
            if read == 0 {
                return Ok(None);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

/// True once the peer closed its end of a daemon connection.
pub(super) fn peer_closed(stream: &UnixStream) -> bool {
    let mut pollfd = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(stream),
        events: libc::POLLRDHUP,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` lives across the call; zero timeout never blocks.
    let ret = unsafe { libc::poll(&mut pollfd, 1, 0) };
    ret > 0
        && pollfd.revents & (libc::POLLHUP | libc::POLLRDHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

pub(super) fn write_frame(writer: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes).and_then(|()| writer.flush())
}
