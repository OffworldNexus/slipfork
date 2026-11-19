//! Typed service diagnostics shared by the entry-target verbs.

use slipfork_edge::EdgeError;
use slipfork_wire::control::{DaemonError, DaemonErrorKind};
use slipfork_world::EngineError;

use super::super::runtime_error;

/// `ServiceNotRunning` diagnostic naming the world and the stopped container.
pub(super) fn service_not_running(world: &str, service: &str) -> EdgeError {
    EdgeError::Control(DaemonError::new(
        DaemonErrorKind::ServiceNotRunning,
        format!(
            "world '{world}' has no running service '{service}' (ServiceNotRunning): psql and \
             redis-cli run only inside the service container — run `slipfork up` and retry"
        ),
    ))
}

/// `ServiceNotRunning` remains a typed daemon error; every other engine failure is internal.
pub(super) fn service_engine_error(world: &str, service: &str, error: EngineError) -> EdgeError {
    match error {
        EngineError::ServiceNotRunning { .. } => service_not_running(world, service),
        other => runtime_error(other.to_string()),
    }
}
