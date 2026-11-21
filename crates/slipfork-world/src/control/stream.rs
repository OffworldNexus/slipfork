//! Streaming control-socket client shared by the supervisor, the daemon, and the live tests.
//!
//! One nonblocking [`UnixStream`] per request: [`StreamClient::poll`] never blocks, so a caller
//! that also watches a client connection (the daemon) can detect a vanished reader and close the
//! world stream instead of leaking a follower. Frame splitting lives in [`FrameLines`]; the
//! optional [`StreamInput`] writer is the one place base64 `ExecInput` frames are encoded.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use base64::Engine as _;

use super::{
    ControlError, FrameLines, InitFrame, OutputKind, PROTOCOL_VERSION, Request, Stream,
    control_socket_path, decode_init_frame, encode,
};
use crate::error::{Error, Result};

/// Longest a frame may grow before the channel is declared corrupt.
const FRAME_CAP: usize = 1 << 20;

/// One streamed output line, already carrying its pod and stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEvent {
    /// Pod that produced the line.
    pub pod: String,
    /// Which stream it came from (`combined` for replayed capture files).
    pub stream: Stream,
    /// Line, divider, or end-of-replay marker.
    pub kind: OutputKind,
    /// Text without its terminator (empty for `eof`).
    pub line: String,
}

/// One frame the init sent for a streaming request.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamFrame {
    /// A line, divider, or eof marker.
    Output(OutputEvent),
    /// The final typed answer.
    Reply {
        /// Result payload on success.
        ok: Option<serde_json::Value>,
        /// Failure detail on error.
        error: Option<ControlError>,
    },
}

/// Nonblocking NDJSON client for one streaming `control.sock` request.
#[derive(Debug)]
pub struct StreamClient {
    socket: UnixStream,
    lines: FrameLines,
    path: PathBuf,
    id: String,
}

impl StreamClient {
    /// Connects to `<pods>/control.sock` and switches the descriptor to nonblocking.
    pub fn open(pods_root: &Path) -> Result<Self> {
        let path = control_socket_path(pods_root)?;
        let socket =
            UnixStream::connect(&path).map_err(|source| Error::io("connect", &path, source))?;
        socket
            .set_nonblocking(true)
            .map_err(|source| Error::io("configure", &path, source))?;
        Ok(Self {
            socket,
            lines: FrameLines::new(),
            path,
            id: String::new(),
        })
    }

    /// Sends one request frame and remembers its id for [`Self::input_writer`].
    pub fn send(&mut self, request: &Request) -> Result<()> {
        self.id = request.id().to_owned();
        let bytes = encode(request)?;
        write_stream(&mut self.socket, &bytes)
            .map_err(|source| Error::io("write", &self.path, source))
    }

    /// A writer for base64 `ExecInput` frames on the same connection.
    pub fn input_writer(&self) -> Result<StreamInput> {
        let socket = self
            .socket
            .try_clone()
            .map_err(|source| Error::io("clone", &self.path, source))?;
        Ok(StreamInput {
            socket,
            id: self.id.clone(),
        })
    }

    /// Reads every complete frame already buffered; `None` means "nothing yet".
    pub fn poll(&mut self) -> Result<Option<StreamFrame>> {
        loop {
            if let Some(frame) = self.take_frame()? {
                return Ok(Some(frame));
            }
            let mut chunk = [0_u8; 8192];
            match self.socket.read(&mut chunk) {
                Ok(0) => {
                    return Err(Error::Init {
                        message: "the world closed its control socket".to_owned(),
                    });
                }
                Ok(read) => self.lines.extend(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(None);
                }
                Err(source) => return Err(Error::io("read", &self.path, source)),
            }
            if self.lines.pending_len() > FRAME_CAP {
                return Err(Error::Protocol {
                    message: "streaming control frame exceeds 1 MiB".to_owned(),
                });
            }
        }
    }

    /// Blocks until the next frame arrives.
    pub fn recv(&mut self) -> Result<StreamFrame> {
        loop {
            if let Some(frame) = self.poll()? {
                return Ok(frame);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Closes the connection; a following init stops streaming on its next write.
    pub fn close(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    fn take_frame(&mut self) -> Result<Option<StreamFrame>> {
        loop {
            let Some(raw) = self.lines.next_line() else {
                return Ok(None);
            };
            let text = std::str::from_utf8(&raw).map_err(|source| Error::Protocol {
                message: format!("control frame is not UTF-8: {source}"),
            })?;
            let frame = decode_init_frame(text)?;
            match frame {
                InitFrame::Output {
                    id,
                    pod,
                    stream,
                    kind,
                    line,
                    ..
                } if id == self.id => {
                    return Ok(Some(StreamFrame::Output(OutputEvent {
                        pod: pod.unwrap_or_default(),
                        stream,
                        kind,
                        line,
                    })));
                }
                InitFrame::Reply { id, ok, error, .. } if id == self.id => {
                    return Ok(Some(StreamFrame::Reply { ok, error }));
                }
                InitFrame::Stopped { .. } => {
                    return Err(Error::Init {
                        message: "the world stopped while a stream was open".to_owned(),
                    });
                }
                InitFrame::Ready { .. } | InitFrame::Reply { .. } | InitFrame::Output { .. } => {
                    continue;
                }
            }
        }
    }
}

/// The write half of a streaming request: base64 `ExecInput` frames on the same socket.
#[derive(Debug)]
pub struct StreamInput {
    socket: UnixStream,
    id: String,
}

impl StreamInput {
    /// Sends one stdin chunk; `None` closes the child's stdin.
    pub fn send(&mut self, data: Option<&[u8]>) -> Result<()> {
        let payload = data.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
        self.send_base64(&payload.unwrap_or_default())
    }

    /// Sends one already-base64-encoded stdin frame; empty data closes the child's stdin.
    pub fn send_base64(&mut self, data: &str) -> Result<()> {
        let request = Request::ExecInput {
            v: PROTOCOL_VERSION,
            id: self.id.clone(),
            data: data.to_owned(),
        };
        let bytes = encode(&request)?;
        write_stream(&mut self.socket, &bytes)
            .map_err(|source| Error::io("write", Path::new("slipfork-world-init stdin"), source))
    }

    /// Shuts the write side down (the world sees EOF on its input pump).
    pub fn close(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Write);
    }
}

fn write_stream(socket: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        match socket.write(&bytes[written..]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    match socket.flush() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(error) => Err(error),
    }
}

/// Replays (and optionally follows) captured pod output.
pub fn logs(
    pods_root: &Path,
    service: Option<&str>,
    follow: bool,
    on_output: &mut dyn FnMut(OutputEvent),
) -> Result<serde_json::Value> {
    let mut client = StreamClient::open(pods_root)?;
    client.send(&Request::Logs {
        v: PROTOCOL_VERSION,
        id: "logs".to_owned(),
        service: service.map(str::to_owned),
        pods: None,
        follow,
    })?;
    loop {
        match client.recv()? {
            StreamFrame::Output(event) => on_output(event),
            StreamFrame::Reply { ok, error } => return finish(ok, error),
        }
    }
}

/// Runs one command inside a live pod and returns its exit code.
pub fn exec(
    pods_root: &Path,
    pod: &str,
    argv: &[String],
    on_output: &mut dyn FnMut(OutputEvent),
) -> Result<i32> {
    let mut client = StreamClient::open(pods_root)?;
    client.send(&Request::Exec {
        v: PROTOCOL_VERSION,
        id: "exec".to_owned(),
        pod: pod.to_owned(),
        argv: argv.to_vec(),
        stdin: false,
    })?;
    loop {
        match client.recv()? {
            StreamFrame::Output(event) => on_output(event),
            StreamFrame::Reply { ok, error } => return exit_code(&finish(ok, error)?),
        }
    }
}

/// Turns the final typed reply into an `ok` payload or a typed error.
pub fn finish(
    ok: Option<serde_json::Value>,
    error: Option<ControlError>,
) -> Result<serde_json::Value> {
    match (error, ok) {
        (Some(error), _) => Err(Error::Protocol {
            message: format!("{}: {}", error.code, error.message),
        }),
        (None, Some(value)) => Ok(value),
        (None, None) => Err(Error::Protocol {
            message: "the world sent an empty streaming reply".to_owned(),
        }),
    }
}

/// Reads the exit code out of a streaming reply payload.
pub fn exit_code(value: &serde_json::Value) -> Result<i32> {
    value
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .ok_or_else(|| Error::Protocol {
            message: "streaming reply carried no exit code".to_owned(),
        })
}
