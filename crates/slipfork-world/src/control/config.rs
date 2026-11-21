//! The `ConfigureS3` client: one request/reply over `<pods>/control.sock`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::{
    FrameLines, InitFrame, PROTOCOL_VERSION, Request, S3WorldConfig, control_socket_path,
    decode_init_frame, encode,
};
use crate::error::{Error, Result};

/// Binds the world's native S3 server and returns its typed reply payload.
///
/// # Errors
///
/// Fails closed when the world is gone, the frame is malformed, or the init refuses the bind.
pub fn configure_s3(pods_root: &Path, config: S3WorldConfig) -> Result<serde_json::Value> {
    let path = control_socket_path(pods_root)?;
    let mut socket =
        UnixStream::connect(&path).map_err(|source| Error::io("connect", &path, source))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|source| Error::io("configure", &path, source))?;
    let id = format!("s3-{}", config.listen_addr.port());
    let request = Request::ConfigureS3 {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        config,
    };
    socket
        .write_all(&encode(&request)?)
        .and_then(|()| socket.flush())
        .map_err(|source| Error::io("write", &path, source))?;
    let mut lines = FrameLines::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if let Some(raw) = lines.next_line() {
            let text = std::str::from_utf8(&raw).map_err(|source| Error::Protocol {
                message: format!("control frame is not UTF-8: {source}"),
            })?;
            match decode_init_frame(text)? {
                InitFrame::Reply {
                    id: reply_id,
                    ok,
                    error,
                    ..
                } if reply_id == id => {
                    return super::stream::finish(ok, error);
                }
                InitFrame::Ready { .. } | InitFrame::Output { .. } => continue,
                InitFrame::Stopped { .. } => {
                    return Err(Error::Init {
                        message: "the world stopped while configuring S3".to_owned(),
                    });
                }
                InitFrame::Reply { .. } => continue,
            }
        }
        let read = socket
            .read(&mut chunk)
            .map_err(|source| Error::io("read", &path, source))?;
        if read == 0 {
            return Err(Error::Protocol {
                message: "the world closed its control socket".to_owned(),
            });
        }
        lines.extend(&chunk[..read]);
    }
}
