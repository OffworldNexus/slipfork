//! Readiness probes for one staged boot node: service ports, PostgreSQL unix sockets, and
//! short-lived liveness checks for portless background workers.
//!
//! Every probe runs in [`Staged::await_ready`] with zero runtime locks held, so PG, Redis, and
//! independent services boot in parallel.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use slipfork_world::control::{dial_tcp, list_pods};
use slipfork_world::{
    BootFailure, BootState, DatabaseBootstrap, ServiceHealth, StepOutcome, classify_service,
};

use crate::world::supervise::BootSink;

/// Window during which an immediate crash of a portless service fails `up`.
///
/// The ceiling has to outlast interpreter startup (`uv run python …` takes ~200ms before it can
/// raise), otherwise a service that dies on its first tick is still `running` when the probe gives
/// up and `up` wrongly reports it ready. The 5ms cadence keeps the settle cheap for healthy pods.
const PORTLESS_WINDOW: Duration = Duration::from_millis(650);

/// A launched node whose readiness is awaited with no runtime lock held.
pub(in crate::world) struct Staged {
    pub(in crate::world) state: BootState,
    pub(in crate::world) detail: String,
    pub(in crate::world) ready: Ready,
    pub(in crate::world) progress: Option<Progress>,
}

/// The long-poll part of one staged node.
pub(in crate::world) enum Ready {
    /// Nothing to wait for (image, setup, hook, native S3).
    None,
    /// A service port inside the world netns.
    Service {
        pods_dir: PathBuf,
        port: u16,
        label: String,
        service: String,
    },
    /// A PostgreSQL unix socket inside the world.
    Unix {
        socket: PathBuf,
        label: String,
        service: String,
        /// Declared child databases, reconciled after the socket answers and before release.
        databases: Option<DatabaseBootstrap>,
    },
    /// A portless worker/probe pod watched briefly for an immediate non-zero crash.
    Portless { pods_dir: PathBuf, service: String },
}

/// One readiness poll's typed sink plus the step it must stay pinned to.
pub(in crate::world) struct Progress {
    sink: BootSink,
}

impl Progress {
    pub(in crate::world) fn new(sink: BootSink) -> Self {
        Self { sink }
    }

    fn frame(&mut self, message: &str) {
        self.sink.progress(message);
    }
}

impl Staged {
    /// Attaches the step-scoped boot sink so a multi-second wait can stream its own progress line.
    pub(in crate::world) fn attach_progress(&mut self, sink: BootSink) {
        self.progress = Some(Progress::new(sink));
    }

    /// Finishes the node once its own readiness probe succeeds.
    pub(in crate::world) fn await_ready(self) -> Result<StepOutcome, BootFailure> {
        let progress = self.progress;
        match self.ready {
            Ready::None => {}
            Ready::Service {
                pods_dir,
                port,
                label,
                service,
            } => wait_tcp(&pods_dir, port, &label, &service, progress)?,
            Ready::Unix {
                socket,
                label,
                service,
                databases,
            } => wait_unix(&socket, &label, &service, databases, progress)?,
            Ready::Portless { pods_dir, service } => wait_portless(&pods_dir, &service)?,
        }
        Ok(StepOutcome {
            state: self.state,
            detail: self.detail,
            run_id: None,
        })
    }
}

fn wait_tcp(
    pods_dir: &Path,
    port: u16,
    label: &str,
    service: &str,
    mut progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut reported = 0_u32;
    while Instant::now() < deadline {
        if let Ok(stream) = dial_tcp(pods_dir, port) {
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(());
        }
        if let Some(progress) = progress.as_mut() {
            report_wait(progress, started, &mut reported, &format!("`{label}`"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Err(BootFailure::with_remedy(
        format!("`{label}` never opened `:{port}` within 10s"),
        format!("run `slipfork logs {service}`"),
    ))
}

fn wait_unix(
    socket: &Path,
    label: &str,
    service: &str,
    databases: Option<DatabaseBootstrap>,
    mut progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut reported = 0_u32;
    let mut last_fault = None;
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            // `connect` succeeds a few milliseconds before postgres accepts queries, so the
            // database reconcile is retried until the socket's own deadline instead of failing
            // the whole `fork` on `the database system is not yet accepting connections`.
            if let Some(databases) = &databases {
                match databases.ensure() {
                    Ok(()) => return Ok(()),
                    Err(fault) => last_fault = Some(fault),
                }
            } else {
                return Ok(());
            }
        }
        if let Some(progress) = progress.as_mut() {
            report_wait(progress, started, &mut reported, &format!("`{label}`"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if let Some(fault) = last_fault {
        return Err(BootFailure::with_remedy(fault.to_string(), fault.remedy()));
    }
    Err(BootFailure::with_remedy(
        format!("`{label}` never opened `{}` within 10s", socket.display()),
        format!("run `slipfork logs {service}`"),
    ))
}

fn wait_portless(pods_dir: &Path, service: &str) -> Result<(), BootFailure> {
    let deadline = Instant::now() + PORTLESS_WINDOW;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        if let Ok(statuses) = list_pods(pods_dir)
            && let Some(status) = statuses.iter().find(|status| status.id == service)
            && (!status.running || status.exit_code.is_some())
        {
            return match classify_service(status.exit_code) {
                ServiceHealth::Exited(0) => Ok(()),
                ServiceHealth::Exited(code) => Err(BootFailure::with_remedy(
                    format!("service `{service}` exited with code {code} during boot"),
                    format!("run `slipfork logs {service}`"),
                )
                .with_exit_code(code)),
                ServiceHealth::Running => Err(BootFailure::with_remedy(
                    format!("service `{service}` exited during boot"),
                    format!("run `slipfork logs {service}`"),
                )),
            };
        }
    }
    Ok(())
}

/// Emits one frame per whole second spent waiting, never a per-2ms poll storm.
fn report_wait(progress: &mut Progress, started: Instant, reported: &mut u32, subject: &str) {
    let elapsed = started.elapsed();
    let whole = u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX);
    if whole <= *reported {
        return;
    }
    *reported = whole;
    progress.frame(&format!(
        "{subject} is still starting ({:.1}s)…",
        elapsed.as_secs_f64()
    ));
}
