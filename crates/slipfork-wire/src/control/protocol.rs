//! The `DaemonRequest` / `DaemonReply` envelope and its typed, secret-free failure.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::request::{
    AuthSummary, DropForkRequest, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary,
    HookRunRequest, LogsRequest, PruneReport, RestartRequest, UpRequest, WorkspaceRefsRequest,
    WorkspaceRefsSummary, WorldExecTarget, WorldSelector,
};
use super::view::{BootEvent, DaemonStatus, WorldProcessReport, WorldSummary, WorldUpSummary};
use slipfork_world::{LogChunk, StreamChunk};

/// Typed reason a control call failed, preserved across the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonErrorKind {
    /// A request that would need the HTTP ingress cannot name one bound port.
    IngressMissing,
    /// The daemon's ingress report has no single real, bound origin port.
    IngressPortInvalid,
    /// The requested world does not exist.
    UnknownWorld,
    /// `restart` was asked for a world this daemon never booted (or already stopped).
    NotRunning,
    /// A requested backing service is not running as a container.
    ServiceNotRunning,
    /// The caller sent a malformed request.
    BadRequest,
    /// The caller sent a request that does not fit the daemon's world model.
    Rejected,
    /// A frame exceeded the 64 KiB cap.
    FrameTooLarge,
    /// The daemon hit an engine or I/O failure.
    Internal,
}

/// A control-call failure with a diagnostic that never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonError {
    /// Typed reason, stable across the wire.
    pub kind: DaemonErrorKind,
    /// Actionable, secret-free diagnostic.
    pub message: String,
}

impl DaemonError {
    /// Build one typed error.
    pub fn new(kind: DaemonErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// One request frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum DaemonRequest {
    /// Daemon process status plus every world (each with exactly one origin).
    Status,
    /// Boot a world; [`DaemonReply::Event`] frames precede the [`DaemonReply::World`] reply.
    Up(UpRequest),
    /// Stop the selected world's components and `crun` services in place, then boot it again;
    /// [`DaemonReply::Event`] frames precede the terminal [`DaemonReply::World`] reply.
    Restart(RestartRequest),
    /// Stop a world's services while keeping its worktree.
    Down(WorldSelector),
    /// Create a fork worktree plus its CoW world.
    Fork(ForkRequest),
    /// Destroy a fork world and its worktree.
    DropFork(DropForkRequest),
    /// List worlds, optionally scoped to one repository root.
    List {
        /// Repository root to scope the listing to; `None` lists every world.
        repo_root: Option<PathBuf>,
    },
    /// Read one world's state, lineage, and process rows in a single call (`slipfork ps`).
    WorldProcesses(WorldSelector),
    /// Preview or reap orphaned worlds and snapshots.
    Prune {
        /// `true` for `--dry-run`.
        dry_run: bool,
    },
    /// Store a `0600` vault secret, refusing to overwrite without `force`.
    SecretSet {
        /// Repository root owning the vault.
        repo_root: PathBuf,
        /// Secret key name.
        key: String,
        /// Secret value (redacted by `Debug`).
        value: ControlSecret,
        /// Rotate an existing value.
        force: bool,
    },
    /// Remove a `0600` vault secret.
    SecretUnset {
        /// Repository root owning the vault.
        repo_root: PathBuf,
        /// Secret key name.
        key: String,
    },
    /// List `0600` vault secret key names.
    SecretList {
        /// Repository root owning the vault.
        repo_root: PathBuf,
    },
    /// Read or follow retained component/hook logs ([`DaemonReply::Log`] frames).
    Logs(LogsRequest),
    /// Fetch the direct-execution target for a world (or a running backing-service container).
    ExecTarget(ExecTargetRequest),
    /// Fetch the direct-execution target for an interactive `hook run` (native TTY).
    HookExecTarget(HookRunRequest),
    /// Enumerate a live fork world's two advertised refs and commit ids (`git-remote-slipfork`).
    WorkspaceRefs(WorkspaceRefsRequest),
    /// Re-run a hook without a TTY ([`DaemonReply::Chunk`] frames, then the exit code).
    HookRun(HookRunRequest),
    /// Bind host listeners and bridge them into a world for this connection's lifetime.
    Forward(ForwardRequest),
    /// Stop serving: the daemon releases `daemon.sock` and exits.
    ///
    /// Used only by `slipfork daemon install` to hand a zero-setup `:0` daemon over to
    /// `slipforkd.socket` (`ADR-0011` §3).
    Shutdown,
    /// Pair this machine against the daemon's boot-injected OSPP endpoint.
    AuthLogin {
        /// Optional endpoint URL from the CLI; the daemon refuses any endpoint that differs
        /// from the one injected at boot (one server identity per daemon).
        endpoint_url: Option<String>,
    },
}

/// One reply frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum DaemonReply {
    /// Reply to [`DaemonRequest::Status`].
    Status {
        /// Daemon status.
        status: DaemonStatus,
    },
    /// A live boot event, streamed before the terminal [`DaemonReply::World`] reply.
    Event {
        /// The boot event.
        event: BootEvent,
    },
    /// Terminal reply to [`DaemonRequest::Up`] / [`DaemonRequest::Restart`] /
    /// [`DaemonRequest::Fork`].
    World {
        /// The booted world.
        world: WorldUpSummary,
    },
    /// Reply to [`DaemonRequest::Down`] / [`DaemonRequest::DropFork`].
    Released {
        /// `true` when a world was released.
        released: bool,
    },
    /// Reply to [`DaemonRequest::List`].
    Worlds {
        /// Every matching world.
        worlds: Vec<WorldSummary>,
    },
    /// Reply to [`DaemonRequest::WorldProcesses`].
    Processes {
        /// The world's state, lineage, and process rows.
        report: WorldProcessReport,
    },
    /// Reply to [`DaemonRequest::Prune`].
    Prune {
        /// What was (or would be) reaped.
        report: PruneReport,
    },
    /// Reply to [`DaemonRequest::SecretSet`].
    SecretSet,
    /// Reply to [`DaemonRequest::SecretUnset`].
    SecretUnset {
        /// `true` when a key was removed.
        removed: bool,
    },
    /// Reply to [`DaemonRequest::SecretList`].
    SecretKeys {
        /// Stored secret key names (never values).
        keys: Vec<String>,
    },
    /// One streamed `hook run` output chunk.
    Chunk {
        /// Captured output chunk.
        chunk: StreamChunk,
    },
    /// One streamed retained log chunk, attributed to its component or hook id.
    Log {
        /// Attributed log chunk.
        log: LogChunk,
    },
    /// Terminal reply to [`DaemonRequest::Logs`].
    Done,
    /// Terminal reply to [`DaemonRequest::HookRun`].
    HookExit {
        /// Hook process exit code.
        exit_code: i32,
    },
    /// Reply to [`DaemonRequest::ExecTarget`].
    ExecTarget {
        /// The direct-execution target.
        target: WorldExecTarget,
    },
    /// Reply to [`DaemonRequest::WorkspaceRefs`].
    WorkspaceRefs {
        /// Advertised refs and their commit ids.
        refs: WorkspaceRefsSummary,
    },
    /// Reply to [`DaemonRequest::Forward`]; the connection then stays open for the bridge.
    Forward {
        /// Bound host listeners.
        summary: ForwardSummary,
    },
    /// Reply to [`DaemonRequest::Shutdown`]; the server stops accepting after this frame.
    Shutdown,
    /// Reply to [`DaemonRequest::AuthLogin`].
    Auth {
        /// Pairing outcome.
        auth: AuthSummary,
    },
    /// The call failed; the connection is closed after this frame.
    Error {
        /// Typed, secret-free failure.
        error: DaemonError,
    },
}

/// A secret value carried by exactly one request variant, with a redacting [`fmt::Debug`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ControlSecret(String);

impl ControlSecret {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the secret value (only the daemon's secret-store call should).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ControlSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
