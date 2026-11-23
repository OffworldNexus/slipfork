//! Minimal PostgreSQL v3 wire client for one pre-fork `CHECKPOINT`.
//!
//! The daemon reaches the pod over `<pods>/control.sock` (`Request::Dial`, `SCM_RIGHTS`) and hands
//! this module a plain `TcpStream`; no `crun`, namespace spawn, or `psql` binary is involved.
//! PostgreSQL buffers a just-committed row in user-space `shared_buffers`/`wal_buffers` until a
//! checkpoint writes it to the kernel page cache, which is exactly what the fork's `syncfs` needs.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::{PG_DATABASE, PG_USER};

/// Every read and write on the dialed connection is bounded so fork never stalls on a hung pod.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);
/// PostgreSQL protocol version 3.0 (`196608`).
const PROTOCOL_VERSION: i32 = 196608;
/// Refuse absurd frame lengths instead of allocating attacker-controlled buffers.
const MAX_FRAME: usize = 1 << 20;

/// One trusted local PostgreSQL session over an already-dialed world-loopback stream.
#[derive(Debug)]
pub struct PostgresSession {
    stream: TcpStream,
}

impl PostgresSession {
    /// Wraps a stream already handed over by the world's `Dial` control request.
    #[must_use]
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// Opens a trust session, runs `CHECKPOINT;`, and returns how long the exchange took.
    ///
    /// # Errors
    ///
    /// Fails closed on a timeout, a malformed/unknown frame, an authentication request other than
    /// `AuthenticationOk` (trust auth is expected on `127.0.0.1`), or a server `ErrorResponse`.
    pub fn checkpoint(mut self) -> Result<Duration> {
        let started = Instant::now();
        self.stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .and_then(|()| self.stream.set_write_timeout(Some(SOCKET_TIMEOUT)))
            .map_err(|source| Error::Protocol {
                message: format!("postgres wire startup: configure socket timeouts: {source}"),
            })?;
        self.startup()?;
        self.simple_query(b"CHECKPOINT;\0")?;
        let _ = self.terminate();
        Ok(started.elapsed())
    }

    /// Sends `StartupMessage` and drains parameter/backend-key frames until `ReadyForQuery`.
    fn startup(&mut self) -> Result<()> {
        let mut body = Vec::with_capacity(40);
        body.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        body.extend_from_slice(b"user\0");
        body.extend_from_slice(PG_USER.as_bytes());
        body.extend_from_slice(b"\0database\0");
        body.extend_from_slice(PG_DATABASE.as_bytes());
        body.extend_from_slice(b"\0\0");
        let mut frame = Vec::with_capacity(body.len() + 4);
        frame.extend_from_slice(&length_of(&body, "startup")?.to_be_bytes());
        frame.extend_from_slice(&body);
        self.send(&frame, "startup")?;
        loop {
            let (kind, body) = self.receive("startup")?;
            match kind {
                b'R' => authenticate(&body)?,
                b'E' => return Err(server_error("startup", &body)),
                b'Z' => return Ok(()),
                // ParameterStatus, BackendKeyData, NoticeResponse, NotificationResponse.
                b'S' | b'K' | b'N' | b'A' => {}
                other => {
                    return Err(Error::Protocol {
                        message: format!(
                            "postgres wire startup: unexpected message type `{}`",
                            char::from(other)
                        ),
                    });
                }
            }
        }
    }

    /// Sends one `SimpleQuery` and waits for `ReadyForQuery`, failing on `ErrorResponse`.
    fn simple_query(&mut self, sql: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(sql.len() + 5);
        frame.push(b'Q');
        frame.extend_from_slice(&length_of(sql, "checkpoint")?.to_be_bytes());
        frame.extend_from_slice(sql);
        self.send(&frame, "checkpoint")?;
        loop {
            let (kind, body) = self.receive("checkpoint")?;
            match kind {
                b'E' => return Err(server_error("checkpoint", &body)),
                b'Z' => return Ok(()),
                _ => {}
            }
        }
    }

    /// Best-effort `Terminate`; the session is already complete when this runs.
    fn terminate(&mut self) -> std::io::Result<()> {
        self.stream.write_all(&[b'X', 0, 0, 0, 4])?;
        self.stream.flush()
    }

    fn send(&mut self, frame: &[u8], stage: &str) -> Result<()> {
        self.stream
            .write_all(frame)
            .and_then(|()| self.stream.flush())
            .map_err(|source| Error::Protocol {
                message: format!("postgres wire {stage}: write: {source}"),
            })
    }

    /// Reads one `type + int32 length + body` frame, with the length including itself.
    fn receive(&mut self, stage: &str) -> Result<(u8, Vec<u8>)> {
        let mut header = [0_u8; 5];
        self.stream
            .read_exact(&mut header)
            .map_err(|source| Error::Protocol {
                message: format!("postgres wire {stage}: read header: {source}"),
            })?;
        let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let Ok(length) = usize::try_from(length) else {
            return Err(Error::Protocol {
                message: format!("postgres wire {stage}: negative frame length"),
            });
        };
        if length < 4 || length - 4 > MAX_FRAME {
            return Err(Error::Protocol {
                message: format!("postgres wire {stage}: invalid frame length {length}"),
            });
        }
        let mut body = vec![0_u8; length - 4];
        self.stream
            .read_exact(&mut body)
            .map_err(|source| Error::Protocol {
                message: format!("postgres wire {stage}: read body: {source}"),
            })?;
        Ok((header[0], body))
    }
}

/// Accepts only `AuthenticationOk`; every real password scheme means the wrong pod config.
fn authenticate(body: &[u8]) -> Result<()> {
    let Some(code) = body
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(i32::from_be_bytes)
    else {
        return Err(Error::Protocol {
            message: "postgres wire startup: short authentication frame".to_owned(),
        });
    };
    if code != 0 {
        return Err(Error::Protocol {
            message: format!(
                "postgres wire startup: server requested authentication code {code}; trust auth on 127.0.0.1 is expected"
            ),
        });
    }
    Ok(())
}

/// Extracts the human-readable `M` field from an `ErrorResponse` body.
fn server_error(stage: &str, body: &[u8]) -> Error {
    let mut message = None;
    let mut rest = body;
    while let Some((&field, tail)) = rest.split_first() {
        if field == 0 {
            break;
        }
        let Some(end) = tail.iter().position(|byte| *byte == 0) else {
            break;
        };
        if field == b'M' {
            message = Some(String::from_utf8_lossy(&tail[..end]).into_owned());
        }
        rest = &tail[end + 1..];
    }
    Error::Protocol {
        message: format!(
            "postgres wire {stage}: server error: {}",
            message.unwrap_or_else(|| "unknown error".to_owned())
        ),
    }
}

/// `int32` length of a message body, including the length word itself.
fn length_of(body: &[u8], stage: &str) -> Result<i32> {
    i32::try_from(body.len() + 4).map_err(|_| Error::Protocol {
        message: format!("postgres wire {stage}: frame exceeds protocol length"),
    })
}
