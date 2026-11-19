//! The typed `DaemonControl` port the thin `slipfork` CLI drives (`ADR-0008` §4).
//!
//! `slipforkd` implements [`DaemonControl`] in-process; [`client::DaemonControlClient`] speaks the
//! same trait over `<runtime_dir>/daemon.sock` (`0600`, bounded 64 KiB JSON-line frames) and
//! [`server::DaemonControlServer`] exposes an injected implementation on that socket.
//!
//! The request/reply/view-model contract these frames carry is owned by
//! [`slipfork_wire::control`] — one definition shared with `slipfork-engine` and the CLI, with no
//! mapping structs. This module owns only the port itself (its failures are [`EdgeError`]s of the
//! edge crate) and the projection of an edge failure into the typed [`DaemonError`] the client
//! sees.

pub mod client;
pub mod server;

use std::path::Path;

use crate::EdgeError;

use slipfork_wire::control::{
    AuthSummary, BootEvent, ControlError, DaemonError, DaemonErrorKind, DaemonStatus,
    DropForkRequest, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary,
    HookRunRequest, LogsRequest, PruneReport, UpRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldProcessReport, WorldSelector, WorldSummary,
    WorldUpSummary,
};
use slipfork_world::{LogChunk, StreamChunk};

impl From<&EdgeError> for DaemonError {
    fn from(err: &EdgeError) -> Self {
        if let EdgeError::Control(error) = err {
            return error.clone();
        }
        let kind = match err {
            EdgeError::IngressMissing => DaemonErrorKind::IngressMissing,
            EdgeError::IngressPortInvalid { .. } => DaemonErrorKind::IngressPortInvalid,
            EdgeError::HostOutsideLocalhost { .. } | EdgeError::IngressTraversal { .. } => {
                DaemonErrorKind::BadRequest
            }
            EdgeError::UnknownHost { .. } | EdgeError::UnknownIngressPath { .. } => {
                DaemonErrorKind::UnknownWorld
            }
            EdgeError::ForwardSpec { .. } => DaemonErrorKind::Rejected,
            EdgeError::ControlFrame { .. } => DaemonErrorKind::FrameTooLarge,
            EdgeError::ControlProtocol { .. } => DaemonErrorKind::BadRequest,
            _ => DaemonErrorKind::Internal,
        };
        Self::new(kind, err.to_string())
    }
}

impl From<EdgeError> for DaemonError {
    fn from(err: EdgeError) -> Self {
        Self::from(&err)
    }
}

/// The complete typed CLI <-> `slipforkd` control contract (`ARCHITECTURE.md` §3.4).
///
/// Implemented in-process by `slipforkd`; [`client::DaemonControlClient`] speaks the identical
/// trait over `<runtime_dir>/daemon.sock`. Streaming calls report progress through their callback
/// and return the terminal value.
pub trait DaemonControl: Send + Sync {
    /// Daemon status plus every world, each with exactly one origin URL.
    fn status(&self) -> Result<DaemonStatus, EdgeError>;

    /// Boot (or idempotently confirm) the world selected by `req.cwd` / `req.world`.
    fn up(
        &self,
        req: &UpRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Stop the selected world's components + `crun` backing services in place — keeping the
    /// worktree and the world's CoW state — then run the exact same boot pipeline as
    /// [`DaemonControl::up`] (`changed: true`, `restarted` naming what came back).
    ///
    /// A `Failed` world re-boots (the rescue path); a world this daemon never booted fails closed
    /// with [`DaemonErrorKind::NotRunning`] and one line naming `slipfork up`.
    fn restart(
        &self,
        req: &WorldSelector,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Stop a world's services while keeping its worktree.
    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError>;

    /// Create `.worktrees/<fork>` plus its CoW world.
    fn fork(
        &self,
        req: &ForkRequest,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldUpSummary, EdgeError>;

    /// Destroy a fork world and its worktree.
    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError>;

    /// List worlds, optionally scoped to one repository root.
    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError>;

    /// Read one world's lifecycle state, build lineage, and every service row in one call.
    ///
    /// The world's `state` comes from the same read as the rows, so `slipfork ps` never needs a
    /// second status call. An unknown or never-booted world fails closed with a typed
    /// `UnknownWorld` error — never an empty report.
    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError>;

    /// Preview or reap orphaned worlds and snapshots.
    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError>;

    /// Store a `0600` vault secret, refusing to overwrite without `force`.
    fn secret_set(
        &self,
        repo_root: &Path,
        key: &str,
        value: &str,
        force: bool,
    ) -> Result<(), EdgeError>;

    /// Remove a `0600` vault secret.
    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError>;

    /// List stored secret key names (never values).
    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError>;

    /// Read or follow retained logs, streaming attributed chunks to `on_chunk`.
    ///
    /// Returning [`std::ops::ControlFlow::Break`] means the peer is gone: the daemon must abort its
    /// reader threads and stop waiting on the world's restart bus instead of accumulating output for
    /// a client that will never read it.
    fn logs(
        &self,
        req: &LogsRequest,
        on_chunk: &mut dyn FnMut(LogChunk) -> std::ops::ControlFlow<()>,
    ) -> Result<(), EdgeError>;

    /// Fetch the direct-execution target (the only `ExecEnv`-bearing reply).
    ///
    /// With `req.service = None` this enters the world rootfs; with `Some(id)` it returns the
    /// `crun exec` entry for that running backing-service container and fails closed with
    /// [`DaemonErrorKind::ServiceNotRunning`] instead of falling back to the app rootfs.
    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError>;

    /// Fetch the direct-execution target for an interactive `hook run` (native TTY).
    fn hook_exec_target(&self, _req: &HookRunRequest) -> Result<WorldExecTarget, EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend does not implement interactive hook entry".to_owned(),
        })
    }

    /// Enumerate a live fork world's refs (the read-only path `git-remote-slipfork` drives).
    fn workspace_refs(
        &self,
        _req: &WorkspaceRefsRequest,
    ) -> Result<WorkspaceRefsSummary, EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend does not implement fork ref enumeration".to_owned(),
        })
    }

    /// Re-run a hook without a TTY, streaming chunks to `on_chunk`, returning its exit code.
    fn hook_run(
        &self,
        req: &HookRunRequest,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<i32, EdgeError>;

    /// Bind host listeners in-process and bridge them into a world.
    ///
    /// `wait_disconnect` publishes the [`ForwardSummary`] to the caller and returns when the
    /// control connection closes, at which point the bridges are torn down.
    fn forward(
        &self,
        req: &ForwardRequest,
        wait_disconnect: &mut dyn FnMut(ForwardSummary),
    ) -> Result<(), EdgeError>;

    /// Pair this machine against the daemon's boot-injected OSPP endpoint.
    ///
    /// `endpoint_url` must be absent or byte-identical to the injected endpoint; the daemon never
    /// dials a second server identity.
    fn auth_login(&self, endpoint_url: Option<&str>) -> Result<AuthSummary, EdgeError>;

    /// Acknowledge a shutdown request; the transport server stops serving after the reply.
    ///
    /// A control backend that cannot stop its transport fails closed (it never pretends).
    fn shutdown(&self) -> Result<(), EdgeError> {
        Err(EdgeError::ControlProtocol {
            reason: "this control backend cannot shut down".to_owned(),
        })
    }
}

/// Project a wire/crate control failure into the typed, secret-free `DaemonError` a client sees.
pub(crate) fn project_error(err: slipfork_wire::control::ControlError) -> DaemonError {
    use slipfork_wire::control::ControlError;
    let kind = match &err {
        ControlError::Io { .. } => DaemonErrorKind::Internal,
        ControlError::Frame { .. } => DaemonErrorKind::FrameTooLarge,
        ControlError::Protocol { .. } => DaemonErrorKind::BadRequest,
        ControlError::Remote(_) => DaemonErrorKind::Internal,
    };
    match err {
        ControlError::Remote(error) => error,
        other => DaemonError::new(kind, other.to_string()),
    }
}

impl From<slipfork_wire::control::IngressError> for EdgeError {
    fn from(err: slipfork_wire::control::IngressError) -> Self {
        use slipfork_wire::control::IngressError;
        match err {
            IngressError::Missing => Self::IngressMissing,
            IngressError::Invalid { reason } => Self::IngressPortInvalid { reason },
        }
    }
}

/// Map a transport-only wire failure into the edge crate's richer error.
pub(crate) fn transport_error(err: ControlError) -> EdgeError {
    match err {
        ControlError::Io { operation, message } => EdgeError::ControlIo { operation, message },
        ControlError::Frame { reason } => EdgeError::ControlFrame { reason },
        ControlError::Protocol { reason } => EdgeError::ControlProtocol { reason },
        ControlError::Remote(error) => EdgeError::Control(error),
    }
}
