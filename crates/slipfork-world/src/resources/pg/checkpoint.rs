//! A `CHECKPOINT` over PostgreSQL's own wire protocol, with no `crun exec` and no client binary.
//!
//! The pod's unix socket is host-visible (`<clone>/.sock/.s.PGSQL.5432`), and the launcher pins
//! `trust` auth, so one startup + simple query flushes the WAL before `fork` snapshots the clone.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};

const PROTOCOL_V3: u32 = 196_608;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Issues one `CHECKPOINT` on the postgres pod's unix socket.
///
/// A socket that is not accepting connections is treated as "no running pod": the caller is
/// `fork`, and a stopped cluster has no dirty pages to flush.
pub fn checkpoint(socket: &Path) -> Result<()> {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return Ok(());
    };
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|source| Error::io("configure", socket, source))?;
    startup(&mut stream, socket)?;
    query(&mut stream, socket, "CHECKPOINT")?;
    Ok(())
}

fn startup(stream: &mut UnixStream, socket: &Path) -> Result<()> {
    let mut params = Vec::new();
    for (key, value) in [
        ("user", "postgres"),
        ("database", "postgres"),
        ("client_encoding", "UTF8"),
    ] {
        params.extend_from_slice(key.as_bytes());
        params.push(0);
        params.extend_from_slice(value.as_bytes());
        params.push(0);
    }
    params.push(0);
    let length = u32::try_from(4 + 4 + params.len()).map_err(|_| protocol(socket, "startup"))?;
    let mut frame = Vec::with_capacity(length as usize);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
    frame.extend_from_slice(&params);
    stream
        .write_all(&frame)
        .map_err(|source| Error::io("write", socket, source))?;
    read_until_ready(stream, socket)
}

fn query(stream: &mut UnixStream, socket: &Path, sql: &str) -> Result<()> {
    let mut payload = sql.as_bytes().to_vec();
    payload.push(0);
    let length = u32::try_from(4 + payload.len()).map_err(|_| protocol(socket, "query"))?;
    let mut frame = Vec::with_capacity(1 + length as usize);
    frame.push(b'Q');
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    stream
        .write_all(&frame)
        .map_err(|source| Error::io("write", socket, source))?;
    read_until_ready(stream, socket)
}

/// Reads backend messages until `ReadyForQuery`; any `ErrorResponse` fails the checkpoint.
fn read_until_ready(stream: &mut UnixStream, socket: &Path) -> Result<()> {
    loop {
        let mut tag = [0_u8; 1];
        stream
            .read_exact(&mut tag)
            .map_err(|source| Error::io("read", socket, source))?;
        let mut length = [0_u8; 4];
        stream
            .read_exact(&mut length)
            .map_err(|source| Error::io("read", socket, source))?;
        let length = u32::from_be_bytes(length);
        let body = usize::try_from(length.saturating_sub(4))
            .map_err(|_| protocol(socket, "message length"))?;
        let mut body = vec![0_u8; body];
        stream
            .read_exact(&mut body)
            .map_err(|source| Error::io("read", socket, source))?;
        match tag[0] {
            b'E' => {
                return Err(Error::Protocol {
                    message: format!(
                        "postgres refused CHECKPOINT on `{}`: {}",
                        socket.display(),
                        error_message(&body)
                    ),
                });
            }
            b'Z' => return Ok(()),
            _ => {}
        }
    }
}

/// Extracts the human `M` field from a PostgreSQL `ErrorResponse`.
fn error_message(body: &[u8]) -> String {
    for field in body.split(|byte| *byte == 0) {
        if let Some(message) = field.strip_prefix(b"M") {
            return String::from_utf8_lossy(message).into_owned();
        }
    }
    "unknown backend error".to_owned()
}

fn protocol(socket: &Path, detail: &str) -> Error {
    Error::Protocol {
        message: format!("postgres {} frame for `{}`", detail, socket.display()),
    }
}
