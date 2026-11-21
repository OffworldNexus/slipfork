//! The `Dial` client: one world-loopback `TcpStream` handed over `SCM_RIGHTS`.

use std::io::Write;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::{ControlError, PROTOCOL_VERSION, Request, control_socket_path, encode};
use crate::error::{Error, Result};

/// Dials `127.0.0.1:<port>` **inside** the world and returns the connection to the caller.
///
/// The pathname socket is opened without a `BufReader`: the init answers with one status byte plus,
/// on success, an `SCM_RIGHTS` descriptor in the same `sendmsg`, and any buffered read would lose
/// the ancillary data. Status `0` carries an fd; status `1` carries a JSON [`ControlError`].
///
/// # Errors
///
/// Fails closed when the control socket is missing, the world refuses the port, or the reply is not
/// one of the two documented status bytes.
pub fn dial_tcp(pods_root: &Path, port: u16) -> Result<TcpStream> {
    if port == 0 {
        return Err(Error::Protocol {
            message: "port 0 is not a dial target".to_owned(),
        });
    }
    let path = control_socket_path(pods_root)?;
    let mut socket =
        UnixStream::connect(&path).map_err(|source| Error::io("connect", &path, source))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|source| Error::io("configure", &path, source))?;
    let request = Request::Dial {
        v: PROTOCOL_VERSION,
        id: format!("dial-{port}"),
        port,
    };
    let bytes = encode(&request)?;
    socket
        .write_all(&bytes)
        .and_then(|()| socket.flush())
        .map_err(|source| Error::io("write", &path, source))?;
    let (payload, descriptor) =
        crate::fdpass::recv_fd(&socket).map_err(|source| Error::io("read", &path, source))?;
    match payload.first().copied() {
        Some(0) => {
            let descriptor = descriptor.ok_or_else(|| Error::Protocol {
                message: format!("dial reply for port {port} carried no descriptor"),
            })?;
            Ok(TcpStream::from(descriptor))
        }
        Some(1) => {
            let text = std::str::from_utf8(&payload[1..]).map_err(|source| Error::Protocol {
                message: format!("dial error frame is not UTF-8: {source}"),
            })?;
            let error: ControlError =
                serde_json::from_str(text.trim()).map_err(|source| Error::Protocol {
                    message: format!("dial error frame is malformed: {source}"),
                })?;
            Err(Error::Protocol {
                message: format!(
                    "the world could not reach 127.0.0.1:{port}: {} ({})",
                    error.message, error.code
                ),
            })
        }
        other => Err(Error::Protocol {
            message: format!("unknown dial status byte {other:?}"),
        }),
    }
}
