//! `<runtime_dir>/daemon.sock` server: bounded JSON-line framing over a `0600` Unix socket.
//!
//! One connection carries exactly one request. Most calls answer with a single terminal frame;
//! `up`/`fork`, `logs`, and `hook_run` may stream [`DaemonReply::Event`] / [`DaemonReply::Chunk`]
//! frames first, and `forward` keeps the connection open until the client disconnects.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use slipfork_engine::StreamChunk;

use crate::EdgeError;
use crate::daemon::wire::{FrameReader, write_chunk, write_reply};
use crate::daemon::{
    BootEvent, CONTROL_SOCKET_FILE, DaemonControl, DaemonReply, DaemonRequest, ForwardSummary,
    WorldUpSummary,
};
use crate::unix_socket::{SocketRole, bind_private};

/// Bound on reading one request frame, so a stalled client cannot pin a thread forever. It is
/// cleared once the request is in hand: `forward` and log streaming legitimately block.
const CONTROL_HEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The `DaemonControl` Unix-socket server bound at `<runtime_dir>/daemon.sock`.
pub struct DaemonControlServer {
    control: Arc<dyn DaemonControl>,
    listener: UnixListener,
    path: PathBuf,
}

impl std::fmt::Debug for DaemonControlServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonControlServer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DaemonControlServer {
    /// Bind `<runtime_dir>/daemon.sock` (`0600`), refusing to steal a live daemon's socket.
    pub fn bind(runtime_dir: &Path, control: Arc<dyn DaemonControl>) -> Result<Self, EdgeError> {
        let path = runtime_dir.join(CONTROL_SOCKET_FILE);
        let listener = bind_private(&path, SocketRole::DaemonControl)?;
        listener
            .set_nonblocking(false)
            .map_err(|err| EdgeError::SocketBind {
                role: SocketRole::DaemonControl.label(),
                path: path.display().to_string(),
                message: format!("cannot switch the socket to blocking mode: {err}"),
            })?;
        Ok(Self {
            control,
            listener,
            path,
        })
    }

    /// Path of the bound control socket.
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Path of the control socket inside an explicit `--runtime-dir` (no environment guessing).
    pub fn socket_path_in(runtime_dir: &Path) -> PathBuf {
        runtime_dir.join(CONTROL_SOCKET_FILE)
    }

    /// Serve control connections forever, one `std::thread` per connection.
    ///
    /// Blocks for the daemon's lifetime. A per-connection failure (client disconnect, malformed
    /// frame) never affects the accept loop.
    pub fn serve(self) -> Result<(), EdgeError> {
        let label = self.path.display().to_string();
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let control = Arc::clone(&self.control);
                    thread::spawn(move || serve_connection(control.as_ref(), stream));
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => {
                    return Err(EdgeError::AcceptFailed {
                        listener: label,
                        message: err.to_string(),
                    });
                }
            }
        }
    }
}

/// Serve one control connection: read one request, answer it, close.
fn serve_connection(control: &dyn DaemonControl, stream: UnixStream) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let _ = stream.set_read_timeout(Some(CONTROL_HEAD_TIMEOUT));
    let mut reader = FrameReader::new(read_half);
    let mut writer = stream;

    let frame = match reader.read_frame() {
        Ok(Some(frame)) => frame,
        // A client that connected and left without a request is not an error.
        Ok(None) => return,
        Err(err) => {
            let _ = write_reply(&mut writer, &DaemonReply::Error { error: err.into() });
            return;
        }
    };
    let _ = writer.set_read_timeout(None);
    let request: DaemonRequest = match serde_json::from_slice(&frame) {
        Ok(request) => request,
        Err(err) => {
            let failure = EdgeError::ControlProtocol {
                reason: err.to_string(),
            };
            let _ = write_reply(
                &mut writer,
                &DaemonReply::Error {
                    error: failure.into(),
                },
            );
            return;
        }
    };

    if let Err(err) = dispatch(control, request, &mut reader, &mut writer) {
        let _ = write_reply(&mut writer, &DaemonReply::Error { error: err.into() });
    }
}

/// Answer one request, streaming intermediate frames before the terminal one.
fn dispatch(
    control: &dyn DaemonControl,
    request: DaemonRequest,
    reader: &mut FrameReader<UnixStream>,
    writer: &mut impl Write,
) -> Result<(), EdgeError> {
    match request {
        DaemonRequest::Status => write_reply(
            writer,
            &DaemonReply::Status {
                status: control.status()?,
            },
        ),
        DaemonRequest::SnowResolve(request) => write_reply(
            writer,
            &DaemonReply::SnowResolved {
                summary: control.snow_resolve(&request)?,
            },
        ),
        DaemonRequest::Up(request) => {
            let summary = stream_events(writer, |on_event| control.up(&request, on_event))?;
            write_reply(writer, &DaemonReply::World { world: summary })
        }
        DaemonRequest::Down(request) => write_reply(
            writer,
            &DaemonReply::Released {
                released: control.down(&request)?,
            },
        ),
        DaemonRequest::Fork(request) => {
            let summary = stream_events(writer, |on_event| control.fork(&request, on_event))?;
            write_reply(writer, &DaemonReply::World { world: summary })
        }
        DaemonRequest::DropFork(request) => write_reply(
            writer,
            &DaemonReply::Released {
                released: control.drop_fork(&request)?,
            },
        ),
        DaemonRequest::List { repo_root } => write_reply(
            writer,
            &DaemonReply::Worlds {
                worlds: control.list(repo_root.as_deref())?,
            },
        ),
        DaemonRequest::Prune { dry_run } => write_reply(
            writer,
            &DaemonReply::Prune {
                report: control.prune(dry_run)?,
            },
        ),
        DaemonRequest::Share(request) => write_reply(
            writer,
            &DaemonReply::Shared {
                shared: control.share(&request)?,
            },
        ),
        DaemonRequest::Unshare(request) => write_reply(
            writer,
            &DaemonReply::Origin {
                origin: control.unshare(&request)?,
            },
        ),
        DaemonRequest::SecretSet {
            repo_root,
            key,
            value,
            force,
        } => {
            control.secret_set(&repo_root, &key, value.expose(), force)?;
            write_reply(writer, &DaemonReply::SecretSet)
        }
        DaemonRequest::SecretUnset { repo_root, key } => write_reply(
            writer,
            &DaemonReply::SecretUnset {
                removed: control.secret_unset(&repo_root, &key)?,
            },
        ),
        DaemonRequest::SecretList { repo_root } => write_reply(
            writer,
            &DaemonReply::SecretKeys {
                keys: control.secret_list(&repo_root)?,
            },
        ),
        DaemonRequest::Logs(request) => {
            stream_chunks(writer, |on_chunk| control.logs(&request, on_chunk))?;
            write_reply(writer, &DaemonReply::Done)
        }
        DaemonRequest::ExecTarget(request) => write_reply(
            writer,
            &DaemonReply::ExecTarget {
                target: control.exec_target(&request)?,
            },
        ),
        DaemonRequest::HookRun(request) => {
            let exit_code = stream_chunks(writer, |on_chunk| control.hook_run(&request, on_chunk))?;
            write_reply(writer, &DaemonReply::HookExit { exit_code })
        }
        DaemonRequest::Forward(request) => control.forward(&request, &mut |summary| {
            wait_for_disconnect(writer, reader, summary);
        }),
        DaemonRequest::AuthLogin { pairing_url } => write_reply(
            writer,
            &DaemonReply::Auth {
                auth: control.auth_login(pairing_url.expose())?,
            },
        ),
    }
}

/// Run a streaming boot call, forwarding every event and failing closed on a broken client.
fn stream_events(
    writer: &mut impl Write,
    call: impl FnOnce(&mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>,
) -> Result<WorldUpSummary, EdgeError> {
    let mut failure: Option<EdgeError> = None;
    let summary = call(&mut |event| {
        if failure.is_none()
            && let Err(err) = write_reply(writer, &DaemonReply::Event { event })
        {
            failure = Some(err);
        }
    })?;
    match failure {
        Some(err) => Err(err),
        None => Ok(summary),
    }
}

/// Run a streaming log/hook call, splitting each chunk under the frame cap.
fn stream_chunks<T>(
    writer: &mut impl Write,
    call: impl FnOnce(&mut dyn FnMut(StreamChunk)) -> Result<T, EdgeError>,
) -> Result<T, EdgeError> {
    let mut failure: Option<EdgeError> = None;
    let value = call(&mut |chunk| {
        if failure.is_none()
            && let Err(err) = write_chunk(writer, chunk)
        {
            failure = Some(err);
        }
    })?;
    match failure {
        Some(err) => Err(err),
        None => Ok(value),
    }
}

/// Publish the forward summary, then block until the control client disconnects.
fn wait_for_disconnect(
    writer: &mut impl Write,
    reader: &mut FrameReader<UnixStream>,
    summary: ForwardSummary,
) {
    if write_reply(writer, &DaemonReply::Forward { summary }).is_err() {
        return;
    }
    let _ = reader.drain_until_close();
}
