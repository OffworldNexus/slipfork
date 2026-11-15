//! `<runtime_dir>/daemon.sock` client: the thin `slipfork` CLI's only view of `slipforkd`.
//!
//! [`DaemonControlClient`] implements the same [`DaemonControl`] trait the daemon implements
//! in-process, one fresh connection per call. Streaming replies (`up`/`fork` events, log chunks)
//! are dispatched to the caller's callback until the terminal frame arrives.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use slipfork_engine::StreamChunk;
use slipfork_world::WorldOrigin;

use crate::EdgeError;
use crate::daemon::server::DaemonControlServer;
use crate::daemon::wire::{FrameReader, unexpected_reply};
use crate::daemon::{
    AuthSummary, BootEvent, DaemonControl, DaemonReply, DaemonRequest, DaemonStatus,
    DropForkRequest, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary,
    HookRunRequest, LogsRequest, MAX_CONTROL_FRAME, PruneReport, SharedWorldInfo, UpRequest,
    WorldExecTarget, WorldSelector, WorldSummary, WorldUpSummary,
};

/// A client bound to `<runtime_dir>/daemon.sock` (never guessing `$XDG_RUNTIME_DIR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlClient {
    socket: PathBuf,
}

impl DaemonControlClient {
    /// Bind to the control socket inside an explicitly supplied `--runtime-dir`.
    pub fn bind(runtime_dir: &Path) -> Self {
        Self {
            socket: DaemonControlServer::socket_path_in(runtime_dir),
        }
    }

    /// Path of the control socket this client dials.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// `true` when a daemon answers on the socket (used by zero-setup auto-spawn).
    pub fn is_served(&self) -> bool {
        UnixStream::connect(&self.socket).is_ok()
    }

    /// Send one request and read frames until the terminal reply.
    fn exchange(
        &self,
        request: &DaemonRequest,
        on_event: &mut dyn FnMut(BootEvent),
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<DaemonReply, EdgeError> {
        let stream = self.connect()?;
        let mut reader = FrameReader::new(self.read_half(&stream)?);
        self.send(&stream, request)?;
        read_terminal(&mut reader, on_event, on_chunk)
    }

    fn call(&self, request: &DaemonRequest) -> Result<DaemonReply, EdgeError> {
        self.exchange(request, &mut |_| {}, &mut |_| {})
    }

    fn connect(&self) -> Result<UnixStream, EdgeError> {
        UnixStream::connect(&self.socket).map_err(|err| EdgeError::ControlIo {
            operation: "connect",
            message: format!("{}: {err}", self.socket.display()),
        })
    }

    fn read_half(&self, stream: &UnixStream) -> Result<UnixStream, EdgeError> {
        stream.try_clone().map_err(|err| EdgeError::ControlIo {
            operation: "connect",
            message: err.to_string(),
        })
    }

    /// Write one `\n`-terminated JSON request frame, refusing anything above the frame cap.
    fn send(&self, stream: &UnixStream, request: &DaemonRequest) -> Result<(), EdgeError> {
        let mut bytes = serde_json::to_vec(request).map_err(|err| EdgeError::ControlFrame {
            reason: err.to_string(),
        })?;
        if bytes.len() > MAX_CONTROL_FRAME {
            return Err(EdgeError::ControlFrame {
                reason: format!(
                    "request frame is {} bytes, above the {MAX_CONTROL_FRAME} byte cap",
                    bytes.len()
                ),
            });
        }
        bytes.push(b'\n');
        let mut writer = self.read_half(stream)?;
        writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .map_err(|err| EdgeError::ControlIo {
                operation: "write request",
                message: err.to_string(),
            })
    }
}

/// Read frames until a terminal one, dispatching streamed events/chunks to the callbacks.
fn read_terminal<R: std::io::Read>(
    reader: &mut FrameReader<R>,
    on_event: &mut dyn FnMut(BootEvent),
    on_chunk: &mut dyn FnMut(StreamChunk),
) -> Result<DaemonReply, EdgeError> {
    loop {
        let frame = reader.read_frame()?.ok_or(EdgeError::ControlIo {
            operation: "read reply",
            message: "the daemon closed the control connection without a reply".to_owned(),
        })?;
        let reply: DaemonReply =
            serde_json::from_slice(&frame).map_err(|err| EdgeError::ControlProtocol {
                reason: err.to_string(),
            })?;
        match reply {
            DaemonReply::Event { event } => on_event(event),
            DaemonReply::Chunk { chunk } => on_chunk(chunk),
            DaemonReply::Error { error } => return Err(EdgeError::Control(error)),
            terminal => return Ok(terminal),
        }
    }
}

impl DaemonControl for DaemonControlClient {
    fn status(&self) -> Result<DaemonStatus, EdgeError> {
        match self.call(&DaemonRequest::Status)? {
            DaemonReply::Status { status } => Ok(status),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn snow_resolve(
        &self,
        req: &crate::daemon::SnowResolveRequest,
    ) -> Result<crate::daemon::SnowResolveSummary, EdgeError> {
        match self.call(&DaemonRequest::SnowResolve(req.clone()))? {
            DaemonReply::SnowResolved { summary } => Ok(summary),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        match self.exchange(&DaemonRequest::Up(req.clone()), on_event, &mut |_| {})? {
            DaemonReply::World { world } => Ok(world),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError> {
        match self.call(&DaemonRequest::Down(req.clone()))? {
            DaemonReply::Released { released } => Ok(released),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError> {
        match self.exchange(&DaemonRequest::Fork(req.clone()), on_event, &mut |_| {})? {
            DaemonReply::World { world } => Ok(world),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError> {
        match self.call(&DaemonRequest::DropFork(req.clone()))? {
            DaemonReply::Released { released } => Ok(released),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError> {
        match self.call(&DaemonRequest::List {
            repo_root: repo_root.map(Path::to_path_buf),
        })? {
            DaemonReply::Worlds { worlds } => Ok(worlds),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError> {
        match self.call(&DaemonRequest::Prune { dry_run })? {
            DaemonReply::Prune { report } => Ok(report),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn share(&self, req: &WorldSelector) -> Result<SharedWorldInfo, EdgeError> {
        match self.call(&DaemonRequest::Share(req.clone()))? {
            DaemonReply::Shared { shared } => Ok(shared),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn unshare(&self, req: &WorldSelector) -> Result<WorldOrigin, EdgeError> {
        match self.call(&DaemonRequest::Unshare(req.clone()))? {
            DaemonReply::Origin { origin } => Ok(origin),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError> {
        match self.call(&DaemonRequest::SecretSet {
            repo_root: repo_root.to_path_buf(),
            key: key.to_owned(),
            value: crate::daemon::ControlSecret::new(value),
            force,
        })? {
            DaemonReply::SecretSet => Ok(()),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError> {
        match self.call(&DaemonRequest::SecretUnset {
            repo_root: repo_root.to_path_buf(),
            key: key.to_owned(),
        })? {
            DaemonReply::SecretUnset { removed } => Ok(removed),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError> {
        match self.call(&DaemonRequest::SecretList {
            repo_root: repo_root.to_path_buf(),
        })? {
            DaemonReply::SecretKeys { keys } => Ok(keys),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<(), EdgeError> {
        match self.exchange(&DaemonRequest::Logs(req.clone()), &mut |_| {}, on_chunk)? {
            DaemonReply::Done => Ok(()),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError> {
        match self.call(&DaemonRequest::ExecTarget(req.clone()))? {
            DaemonReply::ExecTarget { target } => Ok(target),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn hook_exec_target(&self, req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        match self.call(&DaemonRequest::HookExecTarget(req.clone()))? {
            DaemonReply::ExecTarget { target } => Ok(target),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn workspace_refs(
        &self,
        req: &crate::daemon::WorkspaceRefsRequest,
    ) -> Result<crate::daemon::WorkspaceRefsSummary, EdgeError> {
        match self.call(&DaemonRequest::WorkspaceRefs(req.clone()))? {
            DaemonReply::WorkspaceRefs { refs } => Ok(refs),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError> {
        match self.exchange(&DaemonRequest::HookRun(req.clone()), &mut |_| {}, on_chunk)? {
            DaemonReply::HookExit { exit_code } => Ok(exit_code),
            other => Err(unexpected_reply(&other)),
        }
    }

    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError> {
        let stream = self.connect()?;
        let mut reader =
            FrameReader::new(stream.try_clone().map_err(|err| EdgeError::ControlIo {
                operation: "connect",
                message: err.to_string(),
            })?);
        self.send(&stream, &DaemonRequest::Forward(req.clone()))?;
        match read_terminal(&mut reader, &mut |_| {}, &mut |_| {})? {
            DaemonReply::Forward { summary } => {
                // Holding the connection open is the bridge's lifetime contract.
                wait_disconnect(summary);
                Ok(())
            }
            other => Err(unexpected_reply(&other)),
        }
    }

    fn auth_login(&self, pairing_url: &str) -> Result<AuthSummary, EdgeError> {
        match self.call(&DaemonRequest::AuthLogin {
            pairing_url: crate::daemon::ControlSecret::new(pairing_url),
        })? {
            DaemonReply::Auth { auth } => Ok(auth),
            other => Err(unexpected_reply(&other)),
        }
    }
}
