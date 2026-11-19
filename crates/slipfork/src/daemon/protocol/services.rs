//! Backing-service id resolution and the typed service diagnostics behind the protocol verbs.

use slipfork_edge::EdgeError;
use slipfork_forkfile::EvaluatedFork;
use slipfork_wire::control::{DaemonError, DaemonErrorKind};
use slipfork_world::EngineError;

use super::super::{rejected, runtime_error};

/// Resolve one `Forkfile` resource id to the running service handle id and the argv to run.
///
/// `pg_databases.main` selects the `pg_clusters.<cluster>` container and appends `-d main` to
/// `psql`; `redis_clusters.cache` is its own service. Nothing ever falls back to the app rootfs.
pub(super) fn resolve_service_target(
    fork: &EvaluatedFork,
    requested: &str,
    argv: &[String],
) -> Result<(String, Vec<String>), EdgeError> {
    if requested.trim().is_empty() {
        return Err(rejected("a backing-service id must not be empty"));
    }
    let (kind, id) = requested.split_once('.').ok_or_else(|| {
        rejected(format!(
            "invalid backing-service id '{requested}' (expected <kind>.<id>, for example pg_databases.main)"
        ))
    })?;
    match kind {
        "pg_databases" => {
            let database = fork
                .pg_databases()
                .into_iter()
                .find(|database| database.database == id)
                .ok_or_else(|| unknown_service(fork, requested))?;
            let mut argv = argv.to_vec();
            if argv.first().map(String::as_str) == Some("psql")
                && !argv.iter().any(|arg| arg == "-d" || arg == "--dbname")
            {
                argv.push("-d".to_owned());
                argv.push(database.database.clone());
            }
            Ok((format!("pg_clusters.{}", database.cluster), argv))
        }
        "redis_clusters" => {
            if !fork.redis.iter().any(|cluster| cluster.id == id) {
                return Err(unknown_service(fork, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        "pg_clusters" => {
            if !fork.postgres.iter().any(|cluster| cluster.id == id) {
                return Err(unknown_service(fork, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        _ => Err(unknown_service(fork, requested)),
    }
}

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

pub(super) fn unknown_service(fork: &EvaluatedFork, requested: &str) -> EdgeError {
    let mut known: Vec<String> = fork
        .pg_databases()
        .into_iter()
        .map(|database| format!("pg_databases.{}", database.database))
        .chain(
            fork.redis
                .iter()
                .map(|cluster| format!("redis_clusters.{}", cluster.id)),
        )
        .collect();
    known.sort();
    rejected(format!(
        "world declares no backing service '{requested}' (known services: {})",
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    ))
}
