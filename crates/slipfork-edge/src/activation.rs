//! Systemd socket activation boundary and the explicit fd-order listener set.
//!
//! This is the **only** module in the crate allowed to read the process environment
//! (`LISTEN_PID` / `LISTEN_FDS`, `ADR-0008` §5). Measured host facts
//! (`docs/research/tunnel-weaver-and-systemd-facts.md` §5.2): activation descriptors start at
//! `fd 3`, `LISTEN_PID` equals the child pid, and the inherited sockets are **blocking** — so the
//! daemon must make them non-blocking itself.
//!
//! Role assignment never inspects port numbers. It follows the `.socket` order
//! [`crate::unit::render_socket_unit`] renders exactly:
//!
//! | `LISTEN_FDS` | `http_listeners` | `forward_listener` |
//! |---|---|---|
//! | `1` | `[fd 3]` | `None` |
//! | `n >= 2` | `fd 3 ..= fd n+1` (leading) | `fd n+2` (trailing) |
//!
//! Anything else fails closed: a self-bind is never attempted when activation is present, and
//! `--listen` is only considered when **both** activation variables are absent.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::AsRawFd;

use listenfd::ListenFd;

use crate::EdgeError;

/// First descriptor `systemd` hands to a socket-activated service.
pub const ACTIVATION_FIRST_FD: i32 = 3;

/// The listeners `slipforkd` serves for its whole lifetime.
///
/// Every listener is handed back **non-blocking** (`ADR-0008` §5 requires it for adopted sockets,
/// and [`bind_listen`] matches so both modes behave identically); [`crate::HttpIngress`]'
/// synchronous accept loops normalize them to blocking mode.
#[derive(Debug)]
pub struct ListenerSet {
    /// TCP HTTP ingress listeners (for example `127.0.0.1:80` and `[::1]:80`; a single ephemeral
    /// listener under `--listen`).
    pub http_listeners: Vec<TcpListener>,
    /// Planned static forward listener that `weaved` forwards shared hostnames to (`H-008`).
    ///
    /// `None` unless activation supplied a trailing descriptor or `--forward-port <port>` was
    /// passed, so `share` will fail closed once the forward listener is required instead of
    /// silently exposing a world.
    pub forward_listener: Option<TcpListener>,
}

impl ListenerSet {
    /// Port the TCP HTTP ingress is bound to (minted into `WorldOrigin::local`).
    pub fn http_port(&self) -> Result<u16, EdgeError> {
        let listener = self.http_listeners.first().ok_or(EdgeError::NoListener)?;
        listener
            .local_addr()
            .map(|addr| addr.port())
            .map_err(|err| EdgeError::ListenerAddress {
                message: err.to_string(),
            })
    }
}

/// Explicit `--listen <addr>` request, used only when no activation environment is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenConfig {
    /// TCP HTTP ingress address (`127.0.0.1:0` for an ephemeral zero-setup daemon).
    pub addr: SocketAddr,
    /// Optional planned static forward port bound on `127.0.0.1`.
    pub forward_port: Option<u16>,
}

/// Read `LISTEN_PID` / `LISTEN_FDS` once and adopt every inherited descriptor by fd order.
///
/// Returns `Ok(None)` only when **both** variables are absent (the caller then decides between
/// `--listen` and [`EdgeError::NoListener`]). When either variable is present the handover must be
/// complete and belong to this process, otherwise this fails closed with
/// [`EdgeError::ActivationMismatch`] or [`EdgeError::ActivationBroken`]; it never self-binds and
/// never falls back to `--listen`.
///
/// The handover is **single-shot by construction**: `listenfd` clears the two variables while
/// adopting, so descriptors can be taken exactly once per process. Startup code must therefore call
/// [`resolve_listener_set`] (or this function) exactly once, and must treat any error as fatal.
pub fn adopt_systemd_listener() -> Result<Option<ListenerSet>, EdgeError> {
    let handover = Activation::read();
    if !handover.present {
        return Ok(None);
    }
    adopt_handover(&handover).map(Some)
}

/// The activation variables as read from the environment exactly once.
struct Activation {
    /// `true` when `LISTEN_PID` or `LISTEN_FDS` is set.
    present: bool,
    listen_pid: Option<String>,
    listen_fds: Option<String>,
}

impl Activation {
    fn read() -> Self {
        let listen_pid = process_env("LISTEN_PID");
        let listen_fds = process_env("LISTEN_FDS");
        Self {
            present: listen_pid.is_some() || listen_fds.is_some(),
            listen_pid,
            listen_fds,
        }
    }
}

/// Adopt an already-read activation handover; the caller has already decided that it is present.
fn adopt_handover(activation: &Activation) -> Result<ListenerSet, EdgeError> {
    let self_pid = std::process::id();
    let raw_pid = activation
        .listen_pid
        .clone()
        .unwrap_or_else(|| "<unset>".to_owned());
    let advertised_pid = raw_pid.parse::<u32>().ok();
    if advertised_pid != Some(self_pid) {
        return Err(EdgeError::ActivationMismatch {
            listen_pid: raw_pid,
            self_pid,
        });
    }

    let raw_fds = activation
        .listen_fds
        .clone()
        .ok_or_else(|| EdgeError::ActivationBroken {
            reason: format!("LISTEN_PID={self_pid} is set but LISTEN_FDS is unset"),
        })?;
    let count = raw_fds
        .parse::<usize>()
        .map_err(|_| EdgeError::ActivationBroken {
            reason: format!("LISTEN_FDS='{raw_fds}' is not a descriptor count"),
        })?;
    if count == 0 {
        return Err(EdgeError::ActivationBroken {
            reason: "LISTEN_FDS=0 advertises no listener".to_owned(),
        });
    }

    let mut inherited = ListenFd::from_env();
    if inherited.len() != count {
        return Err(EdgeError::ActivationBroken {
            reason: format!(
                "LISTEN_FDS={count} advertises {count} descriptors but {} were handed over",
                inherited.len()
            ),
        });
    }

    let mut listeners = Vec::with_capacity(count);
    for index in 0..count {
        let expected_fd = ACTIVATION_FIRST_FD
            + i32::try_from(index).map_err(|_| EdgeError::ActivationBroken {
                reason: format!("LISTEN_FDS={count} does not fit a descriptor range"),
            })?;
        let listener = inherited
            .take_tcp_listener(index)
            .map_err(|err| EdgeError::ActivationBroken {
                reason: format!("inherited fd {expected_fd} is not a TCP listener: {err}"),
            })?
            .ok_or_else(|| EdgeError::ActivationBroken {
                reason: format!("LISTEN_FDS={count} advertises fd {expected_fd}, which is missing"),
            })?;

        // The handover is positional: fd 3 owns the first role. Reject a shifted descriptor set
        // (for example a `LISTEN_FDS_FIRST_FD` extension) instead of serving the wrong sockets.
        let adopted_fd = listener.as_raw_fd();
        if adopted_fd != expected_fd {
            return Err(EdgeError::ActivationBroken {
                reason: format!(
                    "activation descriptor {index} arrived as fd {adopted_fd}, expected fd {expected_fd}"
                ),
            });
        }

        // Inherited sockets are blocking (`facts.md` §5.2): make them non-blocking immediately,
        // before they can be polled or accepted on.
        listener
            .set_nonblocking(true)
            .map_err(|err| EdgeError::ActivationBroken {
                reason: format!("cannot make inherited fd {adopted_fd} non-blocking: {err}"),
            })?;
        listeners.push(listener);
    }

    Ok(assign_roles(listeners))
}

/// Assign activation roles by descriptor count and order, never by port number.
fn assign_roles(mut listeners: Vec<TcpListener>) -> ListenerSet {
    if listeners.len() == 1 {
        return ListenerSet {
            http_listeners: listeners,
            forward_listener: None,
        };
    }
    // `listeners.len() >= 2`: the trailing descriptor is the planned static forward listener, every
    // leading descriptor is a TCP HTTP ingress listener (the `.socket` order `M2.10` renders).
    let forward_listener = listeners.pop();
    ListenerSet {
        http_listeners: listeners,
        forward_listener,
    }
}

/// Resolve the daemon's listener set: activation first, `--listen` only when both activation
/// variables are absent, otherwise fail closed.
///
/// The activation variables are read **once** here and drive the whole decision, so a broken
/// handover is an error even when `--listen` was also passed: there is deliberately no fallback
/// between the two modes. Because `listenfd` consumes the handover while adopting, this (or
/// [`adopt_systemd_listener`]) must be called exactly once at startup.
pub fn resolve_listener_set(config: Option<&ListenConfig>) -> Result<ListenerSet, EdgeError> {
    let handover = Activation::read();
    if handover.present {
        return adopt_handover(&handover);
    }
    match config {
        Some(config) => bind_listen(config),
        None => Err(EdgeError::NoListener),
    }
}

/// Bind an explicit unprivileged listener set (`--listen <addr>` plus optional `--forward-port`).
///
/// The HTTP ingress listener is always bound; the forward listener exists only when a forward port
/// was requested, so `share` fails closed instead of guessing one.
pub fn bind_listen(config: &ListenConfig) -> Result<ListenerSet, EdgeError> {
    let http = bind_tcp(config.addr)?;
    let forward_listener = match config.forward_port {
        None => None,
        Some(port) => Some(bind_tcp(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?),
    };
    Ok(ListenerSet {
        http_listeners: vec![http],
        forward_listener,
    })
}

fn bind_tcp(addr: SocketAddr) -> Result<TcpListener, EdgeError> {
    let listener = TcpListener::bind(addr).map_err(|err| EdgeError::BindListener {
        addr: addr.to_string(),
        message: err.to_string(),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|err| EdgeError::BindListener {
            addr: addr.to_string(),
            message: err.to_string(),
        })?;
    Ok(listener)
}

/// Read one activation variable without failing on non-UTF-8 values.
fn process_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}
