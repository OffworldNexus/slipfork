//! The world's single control plane: `<pods>/control.sock`, mode `0600`, NDJSON `Request` frames.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use slipfork_world::control::{self as wire, InitFrame};

use crate::pod::InitWorld;
use crate::{Fault, REMEDY_PROTOCOL, REMEDY_RUNTIME, respond};

/// Binds `<pods>/control.sock` mode `0600`, replacing a stale socket from a killed world.
pub(crate) fn bind(pods_root: &Path) -> Result<(UnixListener, PathBuf), Fault> {
    let path = wire::control_socket_path(pods_root)
        .map_err(|source| Fault::cause("control socket path check", REMEDY_RUNTIME, source))?;
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|source| {
        Fault::cause(
            format!("control socket bind at `{}`", path.display()),
            REMEDY_RUNTIME,
            source,
        )
    })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        Fault::cause(
            format!("control socket chmod at `{}`", path.display()),
            REMEDY_RUNTIME,
            source,
        )
    })?;
    listener.set_nonblocking(true).map_err(|source| {
        Fault::cause("control socket nonblocking mode", REMEDY_RUNTIME, source)
    })?;
    Ok((listener, path))
}

/// Serves one NDJSON request over the pathname socket; returns `true` for `shutdown`.
pub(crate) fn handle(listener: &UnixListener, world: &mut InitWorld) -> Result<bool, Fault> {
    let (mut stream, _) = match listener.accept() {
        Ok(accepted) => accepted,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
        Err(source) => {
            return Err(Fault::cause(
                "control socket accept",
                REMEDY_PROTOCOL,
                source,
            ));
        }
    };
    let request = {
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|source| Fault::cause("control socket clone", REMEDY_PROTOCOL, source))?,
        );
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| Fault::cause("control socket read", REMEDY_PROTOCOL, source))?;
        if read == 0 {
            return Ok(false);
        }
        wire::decode_request(line.trim())
            .map_err(|source| Fault::cause("control socket decode", REMEDY_PROTOCOL, source))?
    };
    let (frame, shutdown) = respond(world, request);
    reply(&mut stream, &frame)?;
    Ok(shutdown)
}

fn reply(stream: &mut UnixStream, frame: &InitFrame) -> Result<(), Fault> {
    let bytes = wire::encode(frame)
        .map_err(|source| Fault::cause("control reply encode", REMEDY_PROTOCOL, source))?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| Fault::cause("control reply write", REMEDY_PROTOCOL, source))
}
