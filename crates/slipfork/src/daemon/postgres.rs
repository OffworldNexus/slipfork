//! PostgreSQL provisioning: the base-seed restore, database creation, and the postmaster gate.
//!
//! [`PostgresProvisioner`] owns every `psql`/`pg_restore` invocation the base seed world needs and
//! the [`PostgresReady`] policy that gates them. It borrows the one [`WorldEngine`] and the
//! composition root's readiness policy; the seed-dump stream comes from the [`OsppSupervisor`], so
//! no plaintext dump byte ever lands on host disk.

use super::backing::service_specs;
use super::ospp_supervisor::OsppSupervisor;
use super::service::DaemonService;
use super::{rejected, runtime_error};
use slipfork_dag::{PgSeed, ResourcePlan};
use slipfork_edge::EdgeError;
use slipfork_snow::SnowFile;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    EngineError, ExecEnv, ExecOutput, ProjectSlug, ServiceHandle, WorldEngine, WorldHandle,
};
use std::collections::{BTreeMap, BTreeSet};
use std::thread;
use std::time::Duration;

///
/// `postgres:18` binds the port milliseconds before postmaster leaves `PM_STARTUP`, so the
/// engine's TCP-accept readiness probe is not server readiness: the first `psql` must wait for
/// this server-side gate instead of racing crash recovery.
pub(super) const POSTGRES_READY_DEADLINE: Duration = Duration::from_secs(30);
/// Interval between `pg_isready` probes while the postmaster finishes startup.
pub(super) const POSTGRES_READY_POLL: Duration = Duration::from_millis(100);

/// Injected postmaster-readiness policy for the base seed world's `postgres:18` server.
///
/// [`super::compose_daemon`] defaults it once from [`POSTGRES_READY_DEADLINE`]/
/// [`POSTGRES_READY_POLL`]; the gate measures its own start [`std::time::Instant`], so no clock is
/// injected and no test needs to wait 30 real seconds to exercise the deadline.
pub(super) struct PostgresReady {
    /// Total time the postmaster gets to accept `pg_isready`.
    pub(super) deadline: Duration,
    /// Interval between probes while the postmaster finishes startup.
    pub(super) poll: Duration,
}

impl PostgresReady {
    /// Production bounds for the postmaster-readiness gate.
    pub(super) fn production() -> Self {
        Self {
            deadline: POSTGRES_READY_DEADLINE,
            poll: POSTGRES_READY_POLL,
        }
    }
}

/// The base seed world's PostgreSQL provisioner over one engine and one readiness policy.
pub(super) struct PostgresProvisioner<'a> {
    engine: &'a dyn WorldEngine,
    ready: &'a PostgresReady,
    ospp: &'a OsppSupervisor,
}

impl<'a> PostgresProvisioner<'a> {
    pub(super) fn new(
        engine: &'a dyn WorldEngine,
        ready: &'a PostgresReady,
        ospp: &'a OsppSupervisor,
    ) -> Self {
        Self {
            engine,
            ready,
            ospp,
        }
    }

    /// Start every backing-service container in the seed world and restore the selected dumps.
    pub(super) fn restore_base(
        &self,
        project: &ProjectSlug,
        handle: &WorldHandle,
        snow: &SnowFile,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(), EdgeError> {
        let remote_dumps: BTreeSet<&str> = plan
            .pg_databases
            .iter()
            .filter_map(|database| match &database.seed {
                PgSeed::RemoteDump { .. } => Some(database.id.as_str()),
                PgSeed::CleanInitdb => None,
            })
            .collect();
        let session = if remote_dumps.is_empty() {
            None
        } else {
            Some(self.ospp.session()?.ok_or_else(|| {
                runtime_error(
                    "the selected seed dump requires an authenticated OSPP session; run `slipfork auth` (or `up --no-dump` for a clean database)",
                )
            })?)
        };
        let mut services: BTreeMap<String, ServiceHandle> = BTreeMap::new();
        for spec in service_specs(snow, rootfs)? {
            let service = match self.engine.spawn_service(handle, &spec) {
                Ok(service) => service,
                Err(err) => {
                    let tail = self
                        .engine
                        .service_logs(handle, &spec.id)
                        .map(|logs| {
                            let bytes: Vec<u8> = logs
                                .snapshot()
                                .into_iter()
                                .flat_map(|chunk| chunk.bytes)
                                .collect();
                            let text = String::from_utf8_lossy(&bytes);
                            let lines: Vec<&str> = text.lines().collect();
                            lines[lines.len().saturating_sub(20)..].join("\n")
                        })
                        .unwrap_or_default();
                    return Err(runtime_error(format!(
                        "cannot start backing service '{}' in the base seed world: {err}\n{tail}",
                        spec.id
                    )));
                }
            };
            services.insert(spec.id.clone(), service);
        }
        for database in &snow.resources.pg_databases {
            let Some(service) = services.get(&format!("pg_clusters.{}", database.cluster)) else {
                continue;
            };
            self.ensure_database(service, &database.id)?;
            if !remote_dumps.contains(database.id.as_str()) {
                continue;
            }
            let session = session.as_ref().ok_or_else(|| {
                runtime_error(format!(
                    "the seed dump for '{}' requires an authenticated OSPP session",
                    database.id
                ))
            })?;
            let reader = self
                .ospp
                .dump(session, project, &database.id)
                .map_err(|err| {
                    runtime_error(format!(
                        "cannot stream the seed dump for '{}': {err}",
                        database.id
                    ))
                })?;
            // The decrypted stream is piped straight into `zstd -d | pg_restore` inside the
            // container: no plaintext dump byte ever lands on host disk.
            let restore = format!(
                "zstd -d | pg_restore --no-owner --no-privileges --clean --if-exists -U postgres -h 127.0.0.1 -d {}",
                database.id
            );
            let argv = vec!["sh".to_owned(), "-c".to_owned(), restore];
            let mut stderr = Vec::new();
            let output = self
                .engine
                .exec_in_service_with_input(
                    service,
                    &argv,
                    reader,
                    &ExecEnv::default(),
                    &mut |chunk| {
                        if chunk.source == slipfork_world::StreamSource::Stderr {
                            stderr.extend_from_slice(&chunk.bytes);
                        }
                    },
                )
                .map_err(|err| {
                    runtime_error(format!("cannot restore database '{}': {err}", database.id))
                })?;
            if output.exit_code != 0 {
                let detail = if output.stderr.trim().is_empty() {
                    String::from_utf8_lossy(&stderr).into_owned()
                } else {
                    output.stderr.clone()
                };
                return Err(runtime_error(format!(
                    "restoring database '{}' failed with exit code {}: {}",
                    database.id,
                    output.exit_code,
                    detail.trim()
                )));
            }
            self.exec_service_checked(
                service,
                &[
                    "psql".to_owned(),
                    "-U".to_owned(),
                    "postgres".to_owned(),
                    "-h".to_owned(),
                    "127.0.0.1".to_owned(),
                    "-d".to_owned(),
                    database.id.clone(),
                    "-v".to_owned(),
                    "ON_ERROR_STOP=1".to_owned(),
                    "-c".to_owned(),
                    "CHECKPOINT;".to_owned(),
                ],
                &format!("checkpointing database '{}'", database.id),
            )?;
        }
        Ok(())
    }

    /// Create one declared `snow.yml` database in a freshly `initdb`-ed cluster when absent.
    fn ensure_database(&self, service: &ServiceHandle, database: &str) -> Result<(), EdgeError> {
        validate_database_id(database)?;
        self.wait_for_postgres_ready(service)?;
        let query = format!("SELECT 1 FROM pg_database WHERE datname = '{database}'");
        let existing = self.exec_service_checked(
            service,
            &[
                "psql".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                "-d".to_owned(),
                "postgres".to_owned(),
                "-tAc".to_owned(),
                query,
            ],
            &format!("checking database '{database}'"),
        )?;
        if existing.stdout.trim() == "1" {
            return Ok(());
        }
        self.exec_service_checked(
            service,
            &[
                "createdb".to_owned(),
                "-U".to_owned(),
                "postgres".to_owned(),
                "-h".to_owned(),
                "127.0.0.1".to_owned(),
                database.to_owned(),
            ],
            &format!("creating database '{database}'"),
        )?;
        Ok(())
    }

    /// Block until the world's postmaster accepts queries, bounded by the injected
    /// [`PostgresReady`] policy.
    ///
    /// The engine's `readiness_port` probe only proves the container accepts TCP; `postgres:18`
    /// binds `:5432` milliseconds before postmaster leaves `PM_STARTUP`, so a `psql` issued right
    /// after it intermittently dies with `FATAL: the database system is starting up`. `pg_isready`
    /// runs inside the service container through the same [`WorldEngine::exec_in_service`] path as
    /// every later client, and the last server-side diagnostic is surfaced on timeout.
    fn wait_for_postgres_ready(&self, service: &ServiceHandle) -> Result<(), EdgeError> {
        let argv = [
            "pg_isready",
            "-U",
            "postgres",
            "-h",
            "127.0.0.1",
            "-d",
            "postgres",
        ]
        .map(str::to_owned)
        .to_vec();
        let what = "probing the base seed world's postgres readiness";
        await_postgres_ready(self.ready, || {
            let (output, stderr) = self.exec_service(service, &argv, what)?;
            Ok((
                output.exit_code,
                readiness_detail(&output.stdout, &stderr).to_owned(),
            ))
        })
    }

    /// Run `argv` inside a running backing-service container, failing closed on a non-zero exit.
    fn exec_service_checked(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        what: &str,
    ) -> Result<ExecOutput, EdgeError> {
        let (output, detail) = self.exec_service(service, argv, what)?;
        if output.exit_code != 0 {
            return Err(runtime_error(format!(
                "{what} failed with exit code {}: {}",
                output.exit_code,
                detail.trim()
            )));
        }
        Ok(output)
    }

    /// Run `argv` inside a running backing-service container, whatever the exit code.
    ///
    /// Returns the captured output plus the stderr the caller must surface (the process's own
    /// `stderr`, or the streamed chunks when the engine reports none).
    fn exec_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        what: &str,
    ) -> Result<(ExecOutput, String), EdgeError> {
        let mut stderr = Vec::new();
        let output = self
            .engine
            .exec_in_service(service, argv, &ExecEnv::default(), &mut |chunk| {
                if chunk.source == slipfork_world::StreamSource::Stderr {
                    stderr.extend_from_slice(&chunk.bytes);
                }
            })
            .map_err(|err| match err {
                EngineError::ServiceNotRunning { .. } => runtime_error(format!(
                    "the base seed world's backing service '{}' is not running (ServiceNotRunning); cannot {what}",
                    service.id()
                )),
                other => runtime_error(format!("cannot {what}: {other}")),
            })?;
        let detail = if output.stderr.trim().is_empty() {
            String::from_utf8_lossy(&stderr).into_owned()
        } else {
            output.stderr.clone()
        };
        Ok((output, detail))
    }
}

impl DaemonService {
    /// Start every backing-service container in the seed world and restore the selected dumps.
    pub(super) fn restore_base(
        &self,
        project: &ProjectSlug,
        handle: &WorldHandle,
        snow: &SnowFile,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(), EdgeError> {
        self.postgres()
            .restore_base(project, handle, snow, plan, rootfs)
    }

    fn postgres(&self) -> PostgresProvisioner<'_> {
        PostgresProvisioner::new(
            self.engine.as_ref(),
            &self.config.postgres_ready,
            &self.ospp,
        )
    }
}

/// A `snow.yml` database id is interpolated into `psql` SQL, so it stays a plain identifier.
pub(super) fn validate_database_id(id: &str) -> Result<(), EdgeError> {
    if id.is_empty()
        || id.len() > 63
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(rejected(format!(
            "invalid pg_databases id '{id}': expected 1..=63 [A-Za-z0-9_-] characters"
        )));
    }
    Ok(())
}

/// The last server-side diagnostic a failed postgres readiness probe produced.
///
/// `pg_isready` reports "rejecting connections"/"no response" on stdout; a `psql`-style `FATAL`
/// lands on stderr. Prefer stderr, fall back to stdout, and never invent text.
pub(super) fn readiness_detail<'a>(stdout: &'a str, stderr: &'a str) -> &'a str {
    if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    }
}

/// Poll `probe` until it reports exit code `0`, bounded by the [`PostgresReady`] policy.
///
/// `probe` yields the `pg_isready` exit code plus the last server-side diagnostic. The gate stamps
/// its own start [`std::time::Instant`] and sleeps the policy's plain [`Duration`], so a zero
/// deadline expires on the first rejection without a real 30 s wait.
pub(super) fn await_postgres_ready(
    policy: &PostgresReady,
    mut probe: impl FnMut() -> Result<(i32, String), EdgeError>,
) -> Result<(), EdgeError> {
    let started = std::time::Instant::now();
    loop {
        let (exit_code, detail) = probe()?;
        if exit_code == 0 {
            return Ok(());
        }
        if started.elapsed() >= policy.deadline {
            return Err(runtime_error(format!(
                "the base seed world's postgres server did not accept queries within {}s \
                 (pg_isready exit code {exit_code}): {detail}",
                policy.deadline.as_secs(),
            )));
        }
        thread::sleep(policy.poll);
    }
}
