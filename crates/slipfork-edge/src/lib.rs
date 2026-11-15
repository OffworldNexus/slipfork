//! `slipfork-edge`: the sole `slipforkd` HTTP/WS ingress, the canonical world registry, and the
//! typed `DaemonControl` port the thin `slipfork` CLI drives over `<runtime_dir>/daemon.sock`.
//!
//! This crate owns the edge half of the DI tree assembled by the `slipforkd` Composition Root in
//! `crates/slipfork`, and it is the **only** place in the workspace that speaks HTTP/WebSocket to
//! world backends:
//!
//! ```text
//! slipforkd  --injects-->  dyn DaemonControl   (thin CLI drives this over <runtime_dir>/daemon.sock)
//! slipforkd  --injects-->  WorldRegistry       ((ProjectSlug, WorldTarget) -> WorldOrigin routing)
//! ```
//!
//! - [`activation`]: the systemd socket-activation boundary. Process-environment reads
//!   (`LISTEN_PID` / `LISTEN_FDS`) exist **only** in that module; the rest of the crate receives a
//!   [`activation::ListenerSet`] by value.
//! - [`ingress`]: bounded `httparse` HTTP/1.1 dispatch over `std::thread`-per-connection for both the
//!   TCP ingress listeners and the `0600` pathname `AF_UNIX` `<state_dir>/http-ingress.sock` that
//!   `slipfork-world-init` bridges in-netns `$BASE_URL` traffic to (`H-011` #4 Variant a).
//! - [`registry`]: canonical owner of `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing. It
//!   resolves `*.localhost` requests through each world's [`registry::IngressRoute`]
//!   longest-prefix table into `WorldEngine::dial_world_tcp`, proxies `shared_hosts` public
//!   hostnames, enforces the `308`/`421` rules for shared worlds (`ADR-0005` §1), and owns
//!   [`registry::WorldRegistry::share_world`] / [`registry::WorldRegistry::unshare_world`].
//! - [`tunnel`]: [`tunnel::UnavailableTunnelRegistrar`], the fail-closed `TunnelRegistrar` wired
//!   until `weaved` publishes its IPC specification (`H-006` / `M2.22`).
//! - [`daemon`]: the complete typed `DaemonControl` port plus its bounded (64 KiB JSON-line)
//!   `0600` Unix-socket server/client (`ADR-0008` §4).
//! - [`forward`]: daemon-owned `127.0.0.1:0` raw TCP bridges into a world (`H-010` #1).
//! - [`unix_socket`]: the single `0600` pathname-socket binder shared by both listeners.
//! - [`mod@unit`]: the pure non-template `slipforkd.socket` / `slipforkd.service` renderer (`M2.10`,
//!   `ADR-0008` §5): typed [`unit::UnitSpec`], fail-closed [`unit::UnitSpec::validate`], and the two
//!   byte-exact golden-tested renderers.
//!
//! Every module is synchronous (`std::thread`, zero async runtime), and the crate is
//! `#![forbid(unsafe_code)]` (`listenfd` / `httparse` are both safe wrappers).

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use thiserror::Error;

pub mod activation;
pub mod daemon;
pub mod forward;
pub mod ingress;
pub mod registry;
pub(crate) mod relay;
pub mod tunnel;
pub mod unit;
pub mod unix_socket;

pub use activation::{
    ListenConfig, ListenerSet, adopt_systemd_listener, bind_listen, resolve_listener_set,
};
pub use daemon::client::DaemonControlClient;
pub use daemon::server::DaemonControlServer;
pub use daemon::{
    AuthSummary, BootEvent, DaemonControl, DaemonError, DaemonErrorKind, DaemonStatus,
    DropForkRequest, DumpRequest, ExecTargetRequest, ForkRequest, ForwardRequest, ForwardSummary,
    ForwardTarget, HookRunRequest, LogsRequest, PruneReport, ResourceRequest, ResourceSummary,
    UpRequest, WorldExecTarget, WorldSelector, WorldState, WorldSummary, WorldUpSummary,
};
pub use forward::{ForwardBridge, ForwardTargetSpec};
pub use ingress::HttpIngress;
pub use registry::{
    IngressRoute, RouteDecision, RoutedWorld, SharedWorldInfo, WorldRegistry, world_id,
};
pub use tunnel::UnavailableTunnelRegistrar;
pub use unit::{UnitSpec, render_service_unit, render_socket_unit};

/// Fail-closed errors produced by the edge crate.
///
/// Every request-path variant names the world id and the `Host` it was routed for, so a diagnostic
/// never needs the daemon's log context to be actionable. No variant carries a secret value.
#[derive(Debug, Error)]
pub enum EdgeError {
    /// `LISTEN_PID` does not name this process, so the inherited descriptors are not ours.
    #[error(
        "systemd socket activation does not belong to this process: LISTEN_PID={listen_pid} but this pid is {self_pid}; refusing to adopt the inherited descriptors"
    )]
    ActivationMismatch {
        /// Raw `LISTEN_PID` value (`<unset>` when the variable is missing).
        listen_pid: String,
        /// This process's pid.
        self_pid: u32,
    },

    /// The activation environment is present but does not describe a usable listener set.
    #[error("systemd socket activation is malformed: {reason}")]
    ActivationBroken {
        /// What specifically is wrong with the handover.
        reason: String,
    },

    /// Neither socket activation nor an explicit `--listen` address was provided.
    #[error(
        "slipforkd has no HTTP ingress listener: pass --listen <addr> for an unprivileged run, or start slipforkd from the slipforkd.socket unit"
    )]
    NoListener,

    /// An explicit `--listen` address could not be bound.
    #[error("cannot bind the ingress listener at {addr}: {message}")]
    BindListener {
        /// Requested address.
        addr: String,
        /// Underlying OS error.
        message: String,
    },

    /// A bound listener no longer reports a local address.
    #[error("cannot read the bound ingress address: {message}")]
    ListenerAddress {
        /// Underlying OS error.
        message: String,
    },

    /// A user-private pathname socket does not fit the kernel's `AF_UNIX` pathname budget.
    #[error(
        "{role} socket path '{path}' is longer than the {limit}-byte AF_UNIX limit; move the directory it lives in to a shorter path"
    )]
    SocketPath {
        /// Which daemon-owned socket (`http ingress` / `daemon control`).
        role: &'static str,
        /// Full socket path.
        path: String,
        /// `sun_path` budget.
        limit: usize,
    },

    /// A live `slipforkd` already serves this socket path.
    #[error(
        "{role} socket '{path}' is already served by a running slipforkd; refusing to steal it"
    )]
    SocketInUse {
        /// Which daemon-owned socket (`http ingress` / `daemon control`).
        role: &'static str,
        /// Socket path.
        path: String,
    },

    /// A pathname socket could not be created, secured, or removed.
    #[error("cannot prepare the {role} socket '{path}': {message}")]
    SocketBind {
        /// Which daemon-owned socket (`http ingress` / `daemon control`).
        role: &'static str,
        /// Socket path.
        path: String,
        /// Underlying OS error.
        message: String,
    },

    /// The directory holding a pathname socket could not be created.
    #[error("cannot create the {role} directory '{path}': {message}")]
    SocketDir {
        /// Which daemon-owned socket (`http ingress` / `daemon control`).
        role: &'static str,
        /// Directory.
        path: String,
        /// Underlying OS error.
        message: String,
    },

    /// An ingress listener stopped accepting connections.
    #[error("ingress listener '{listener}' stopped accepting connections: {message}")]
    AcceptFailed {
        /// Listener label (TCP address or socket path).
        listener: String,
        /// Underlying OS error.
        message: String,
    },

    /// An ingress accept thread panicked; the daemon must not keep serving a partial listener set.
    #[error("ingress failed: {message}")]
    IngressLoop {
        /// What failed.
        message: String,
    },

    /// The registry lock was poisoned by a panicking thread.
    #[error(
        "world registry is unavailable: {operation} failed because another thread panicked while holding the registry lock"
    )]
    RegistryUnavailable {
        /// Operation that needed the lock.
        operation: &'static str,
    },

    /// A second active world was attached for the same `(project, target)`.
    #[error(
        "world {world} is already registered; release it before attaching another handle for the same project and target"
    )]
    WorldAlreadyRegistered {
        /// World id.
        world: String,
    },

    /// A world's `IngressRoute` table cannot be served.
    #[error("world {world} has an invalid ingress table: {reason}")]
    InvalidIngressRoute {
        /// World id.
        world: String,
        /// What is wrong with the table.
        reason: String,
    },

    /// A `(project, target)` pair cannot be expressed as a `.localhost` route.
    #[error("world {world} has no routable .localhost identity: {reason}")]
    WorldRoute {
        /// World id.
        world: String,
        /// Underlying world error.
        reason: String,
    },

    /// No attached world owns the requested `Host`.
    #[error("no world is attached for host '{host}'")]
    UnknownHost {
        /// Offending `Host` header.
        host: String,
    },

    /// The world is attached, but no `IngressRoute` prefix covers the request path.
    #[error("world {world} has no ingress route matching path '{path}' (host '{host}')")]
    UnknownIngressPath {
        /// World id.
        world: String,
        /// `Host` header the request was routed with.
        host: String,
        /// Request path and query.
        path: String,
    },

    /// `WorldEngine::dial_world_tcp` could not open the world-local backend port.
    #[error("world {world} cannot reach its backend on port {port} for host '{host}': {reason}")]
    UpstreamDial {
        /// World id.
        world: String,
        /// `Host` header the request was routed with.
        host: String,
        /// Backend port inside the world.
        port: u16,
        /// Underlying engine error.
        reason: String,
    },

    /// The world's single public origin could not be minted.
    #[error("cannot mint the origin for world {world} (host '{host}'): {reason}")]
    Origin {
        /// World id.
        world: String,
        /// Route host (or the rejected lease URL).
        host: String,
        /// Underlying world error.
        reason: String,
    },

    /// `slipforkd` has no planned static forward listener, so no tunnel can reach this world.
    #[error(
        "slipforkd has no forward listener, so it cannot share a world: start it from the slipforkd.socket unit, or pass --listen <addr> --forward-port <port>"
    )]
    NoForwardListener,

    /// The tunnel registrar could not mint a public lease.
    #[error("cannot share world {world}: {reason}")]
    TunnelUnavailable {
        /// World id.
        world: String,
        /// Underlying registrar error.
        reason: String,
    },

    /// Two worlds claim the same public tunnel hostname.
    #[error("cannot share world {world}: public host '{host}' is already owned by another world")]
    SharedHostConflict {
        /// World id.
        world: String,
        /// Public hostname returned by the registrar.
        host: String,
    },

    /// Destroying a released world failed.
    #[error("released world {world} could not be destroyed: {reason}")]
    Release {
        /// World id.
        world: String,
        /// Underlying engine error.
        reason: String,
    },

    /// A daemon-owned `forward` request cannot be served as specified.
    #[error("cannot forward: {reason}")]
    ForwardSpec {
        /// What is wrong with the request.
        reason: String,
    },

    /// A `slipforkd.service` unit spec has no usable account name.
    #[error(
        "slipforkd.service needs an explicit developer account name, got '{user}': pass the resolved account (empty, whitespace, and systemd %-template values are refused)"
    )]
    UnitInvalidUser {
        /// The offending `UnitSpec::user`.
        user: String,
    },

    /// A `slipforkd.service` path cannot be written into a UTF-8 systemd unit line.
    #[error(
        "cannot write the {role} path '{path}' into slipforkd.service: unit files are UTF-8 text without control characters"
    )]
    UnitInvalidPath {
        /// Which `ExecStart=` argument the path feeds (`ExecStart`, `--state-dir`, `--runtime-dir`).
        role: &'static str,
        /// The offending path.
        path: String,
    },

    /// A `slipforkd.socket` unit spec has no `ListenStream=` address.
    #[error(
        "slipforkd.socket needs at least one ListenStream address: pass 127.0.0.1:80 (and [::1]:80) as the ingress listeners"
    )]
    UnitNoListener,

    /// The planned forward port is also an ingress port, so the activation fd order is ambiguous.
    #[error(
        "the forward port {forward_port} collides with the ingress listener {listener}: the forward listener must use a different port so fd order stays unambiguous"
    )]
    UnitPortCollision {
        /// The planned static forward port.
        forward_port: u16,
        /// The ingress listener that already uses that port.
        listener: SocketAddr,
    },

    /// The control socket could not be read, written, or connected.
    #[error("{operation} failed on the daemon control socket: {message}")]
    ControlIo {
        /// What was attempted (`connect`, `read request`, `write reply`).
        operation: &'static str,
        /// Underlying OS error.
        message: String,
    },

    /// A control frame exceeded the 64 KiB JSON-line cap or could not be encoded.
    #[error("invalid daemon control frame: {reason}")]
    ControlFrame {
        /// What is wrong with the frame.
        reason: String,
    },

    /// A control peer sent a frame that is not a valid `DaemonRequest`/`DaemonReply`.
    #[error("daemon control protocol error: {reason}")]
    ControlProtocol {
        /// What is wrong with the message.
        reason: String,
    },

    /// The daemon answered with a typed remote error.
    #[error("{0}")]
    Control(DaemonError),
}
