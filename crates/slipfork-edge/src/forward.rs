//! Daemon-owned `forward` bridges: host `127.0.0.1:0` listeners piped into a world (`H-010` #1).
//!
//! `slipfork forward` never receives a file descriptor from `slipforkd` and never speaks a
//! stream-upgrade protocol: the daemon binds the host listener **in-process**, bridges every
//! accepted connection to `WorldEngine::dial_world_tcp` inside the daemon, and keeps the bridge
//! alive exactly as long as the [`ForwardBridge`] value (one open `DaemonControl` connection).
//! Dropping the bridge stops the accept loops and closes every host port.

use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use slipfork_world::{WorldEngine, WorldHandle};

use crate::EdgeError;
use slipfork_wire::control::{ForwardSummary, ForwardTarget};
use slipfork_wire::relay::pipe;

/// How often a non-blocking accept loop re-checks its stop flag (bounds teardown latency).
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// One host listener the daemon must bind and bridge into a world-local port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardTargetSpec {
    /// Stable target id printed back to the caller (`pg_databases.main`).
    pub id: String,
    /// Port bound on `127.0.0.1` inside the world network namespace.
    pub port: u16,
    /// URI scheme rendered into the printed URI (`postgres`, `redis`, `http`).
    pub scheme: String,
    /// URI path appended after the host address (`/main`, or empty).
    pub path: String,
}

impl ForwardTargetSpec {
    /// Fail closed on a target that cannot yield an honest host listener and printable URI.
    pub fn validate(&self) -> Result<(), EdgeError> {
        if self.id.is_empty() || self.id.contains(char::is_whitespace) {
            return Err(EdgeError::ForwardSpec {
                reason: format!(
                    "target id '{}' must be non-empty and contain no whitespace",
                    self.id
                ),
            });
        }
        if self.port == 0 {
            return Err(EdgeError::ForwardSpec {
                reason: format!("target '{}' maps to port 0", self.id),
            });
        }
        let mut scheme = self.scheme.chars();
        let valid_scheme = scheme.next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if !valid_scheme {
            return Err(EdgeError::ForwardSpec {
                reason: format!(
                    "target '{}' has an invalid URI scheme '{}'",
                    self.id, self.scheme
                ),
            });
        }
        if !self.path.is_empty()
            && (!self.path.starts_with('/') || self.path.contains(char::is_whitespace))
        {
            return Err(EdgeError::ForwardSpec {
                reason: format!(
                    "target '{}' path '{}' must be empty or start with '/' and contain no whitespace",
                    self.id, self.path
                ),
            });
        }
        Ok(())
    }
}

/// The daemon-owned host listeners of one open `DaemonControl::forward` call.
pub struct ForwardBridge {
    summary: ForwardSummary,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl ForwardBridge {
    /// Bind one `127.0.0.1:0` listener per spec and start bridging it into `world`.
    ///
    /// A bind failure drops the partially bound bridge (the `?` returns and [`Drop`] stops the
    /// already-running loops), so a failed `forward` never leaves a host port open.
    pub(crate) fn bind(
        engine: Arc<dyn WorldEngine>,
        world: WorldHandle,
        world_id: String,
        specs: &[ForwardTargetSpec],
    ) -> Result<Self, EdgeError> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut bridge = Self {
            summary: ForwardSummary {
                world: world_id,
                targets: Vec::new(),
            },
            stop: Arc::clone(&stop),
            threads: Vec::new(),
        };
        for spec in specs {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|err| {
                EdgeError::BindListener {
                    addr: "127.0.0.1:0".to_owned(),
                    message: format!("cannot bind a forward listener for '{}': {err}", spec.id),
                }
            })?;
            let addr = listener
                .local_addr()
                .map_err(|err| EdgeError::ListenerAddress {
                    message: err.to_string(),
                })?;
            listener
                .set_nonblocking(true)
                .map_err(|err| EdgeError::BindListener {
                    addr: addr.to_string(),
                    message: err.to_string(),
                })?;
            bridge.threads.push(spawn_accept_loop(
                listener,
                Arc::clone(&engine),
                world.clone(),
                spec.port,
                Arc::clone(&stop),
            ));
            bridge.summary.targets.push(ForwardTarget {
                id: spec.id.clone(),
                host_addr: addr.to_string(),
                uri: format!("{}://{}{}", spec.scheme, addr, spec.path),
            });
        }
        Ok(bridge)
    }

    /// The `{world, targets:[{id, host_addr, uri}]}` reply rendered before the bridge blocks.
    pub fn summary(&self) -> ForwardSummary {
        self.summary.clone()
    }
}

impl Drop for ForwardBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// One non-blocking accept loop that owns its listener and closes it when it exits.
fn spawn_accept_loop(
    listener: TcpListener,
    engine: Arc<dyn WorldEngine>,
    world: WorldHandle,
    port: u16,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((client, _)) => {
                    let engine = Arc::clone(&engine);
                    let world = world.clone();
                    thread::spawn(move || bridge(engine.as_ref(), &world, port, client));
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    thread::sleep(ACCEPT_POLL);
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                // The listener is unusable; the host port is closed when this thread returns.
                Err(_) => return,
            }
        }
    })
}

/// Pipe one accepted host connection to `127.0.0.1:<port>` inside the world.
///
/// A raw TCP forward has no error channel of its own: a failed world dial closes the accepted
/// connection (the failure was already surfaced by `forward` when it validated the world).
fn bridge(engine: &dyn WorldEngine, world: &WorldHandle, port: u16, mut client: TcpStream) {
    let Ok(mut upstream) = engine.dial_world_tcp(world, port) else {
        return;
    };
    let _ = pipe(&mut client, &mut upstream);
}
