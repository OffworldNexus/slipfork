//! `slipfork-edge`: the sole `slipforkd` HTTP/WS ingress and the canonical world registry (the
//! daemon control port lands in `M2.9`).
//!
//! This crate will own the edge half of the DI tree assembled by the `slipforkd` Composition Root
//! in `crates/slipfork`, and it is the **only** place in the workspace that speaks HTTP/WebSocket
//! to world backends. Target contract of the crate:
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
//! - [`registry`]: canonical owner of `(ProjectSlug, WorldTarget)` -> `WorldOrigin` routing,
//!   resolving `*.localhost` requests through each world's [`registry::IngressRoute`]
//!   longest-prefix table into `WorldEngine::dial_world_tcp`.
//! - `daemon` / `tunnel` / `unit`: empty placeholders for the `<runtime_dir>/daemon.sock` control
//!   port (`M2.9`), the `TunnelRegistrar` adapters (`M2.9`/`M2.22`), and the unit renderer (`M2.10`).
//!
//! Every module is synchronous (`std::thread`, zero async runtime), and the crate is
//! `#![forbid(unsafe_code)]` (`listenfd` / `socket2` / `httparse` are all safe wrappers).

#![forbid(unsafe_code)]

use thiserror::Error;

pub mod activation;
pub mod daemon;
pub mod ingress;
pub mod registry;
pub mod tunnel;
pub mod unit;

pub use activation::{
    ListenConfig, ListenerSet, adopt_systemd_listener, bind_listen, resolve_listener_set,
};
pub use ingress::HttpIngress;
pub use registry::{IngressRoute, RoutedWorld, WorldRegistry};

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

    /// `<state_dir>/http-ingress.sock` does not fit the kernel's `AF_UNIX` pathname budget.
    #[error(
        "http ingress socket path '{path}' is longer than the {limit}-byte AF_UNIX limit; move --state-dir to a shorter path"
    )]
    IngressSocketPath {
        /// Full socket path.
        path: String,
        /// `sun_path` budget.
        limit: usize,
    },

    /// A live `slipforkd` already serves `<state_dir>/http-ingress.sock`.
    #[error(
        "http ingress socket '{path}' is already served by a running slipforkd; refusing to steal it"
    )]
    IngressSocketInUse {
        /// Socket path.
        path: String,
    },

    /// The `AF_UNIX` ingress socket could not be created, secured, or removed.
    #[error("cannot prepare the http ingress socket '{path}': {message}")]
    IngressSocket {
        /// Socket path.
        path: String,
        /// Underlying OS error.
        message: String,
    },

    /// The state directory holding the ingress socket could not be created.
    #[error("cannot create the slipforkd state directory '{path}': {message}")]
    IngressStateDir {
        /// State directory.
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
        /// Route host.
        host: String,
        /// Underlying world error.
        reason: String,
    },

    /// Destroying a released world failed.
    #[error("released world {world} could not be destroyed: {reason}")]
    Release {
        /// World id.
        world: String,
        /// Underlying engine error.
        reason: String,
    },
}
