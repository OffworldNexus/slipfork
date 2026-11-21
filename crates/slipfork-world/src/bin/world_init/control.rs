//! The world's single control plane: `<pods>/control.sock`, mode `0600`, NDJSON `Request` frames.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::control::{self as wire, InitFrame};

use crate::pod::InitWorld;
use crate::stream::{serve_exec, serve_logs};
use crate::{Fault, REMEDY_PROTOCOL, REMEDY_RUNTIME, respond};

/// Longest a dial may block before the world reports a refused port.
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);

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
    // A dial reply is a status byte plus an optional descriptor, not an NDJSON `InitFrame`.
    match request {
        wire::Request::Dial { port, .. } => {
            dial(&mut stream, port)?;
            Ok(false)
        }
        wire::Request::Logs {
            id,
            service,
            follow,
            ..
        } => {
            let shared = world.shared();
            let out = stream.try_clone().map_err(|source| {
                Fault::cause("streaming socket clone", REMEDY_PROTOCOL, source)
            })?;
            std::thread::spawn(move || {
                if let Err(fault) = serve_logs(shared, id, service, follow, out) {
                    eprintln!("slipfork-world-init: {fault}");
                }
            });
            Ok(false)
        }
        wire::Request::Exec {
            id,
            pod,
            argv,
            stdin,
            ..
        } => {
            let shared = world.shared();
            let out = stream.try_clone().map_err(|source| {
                Fault::cause("streaming socket clone", REMEDY_PROTOCOL, source)
            })?;
            std::thread::spawn(move || {
                if let Err(fault) = serve_exec(shared, id, pod, argv, stdin, out) {
                    eprintln!("slipfork-world-init: {fault}");
                }
            });
            Ok(false)
        }
        request => {
            let (frame, shutdown) =
                respond(world, request, &mut |frame| reply(&mut stream, &frame))?;
            reply(&mut stream, &frame)?;
            Ok(shutdown)
        }
    }
}

/// Connects to `127.0.0.1:<port>` inside this world netns and hands the fd to the client.
///
/// Success is one `sendmsg` carrying payload `[0]` plus the descriptor; a refused or unreachable
/// port is payload `[1]` plus one JSON `ControlError`, so the daemon never invents an fd.
fn dial(stream: &mut UnixStream, port: u16) -> Result<(), Fault> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    match TcpStream::connect_timeout(&address, DIAL_TIMEOUT) {
        Ok(upstream) => slipfork_world::fdpass::send_fd(stream, &[0], Some(upstream.as_fd()))
            .map_err(|source| Fault::cause("dial descriptor reply", REMEDY_PROTOCOL, source)),
        Err(source) => {
            let error = slipfork_world::ControlError {
                code: "dial-refused".to_owned(),
                message: format!("cannot connect to 127.0.0.1:{port}: {source}"),
            };
            let mut payload = vec![1_u8];
            payload.extend(
                wire::encode(&error)
                    .map_err(|error| Fault::cause("dial error encode", REMEDY_PROTOCOL, error))?,
            );
            slipfork_world::fdpass::send_fd(stream, &payload, None)
                .map_err(|source| Fault::cause("dial error reply", REMEDY_PROTOCOL, source))
        }
    }
}

fn reply(stream: &mut UnixStream, frame: &InitFrame) -> Result<(), Fault> {
    let bytes = wire::encode(frame)
        .map_err(|source| Fault::cause("control reply encode", REMEDY_PROTOCOL, source))?;
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|source| Fault::cause("control reply write", REMEDY_PROTOCOL, source))
}
