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
use slipfork_world::pod_log::{self, LogKind, LogRange};
use slipfork_world::{
    BootFailure, BootState, DatabaseBootstrap, ServiceHealth, StepOutcome, Stream, classify_service,
};

use crate::world::supervise::BootSink;

/// Window during which an immediate crash of a portless service fails `up`.
///
/// The ceiling has to outlast interpreter startup (`uv run python …` takes ~200ms before it can
/// raise), otherwise a service that dies on its first tick is still `running` when the probe gives
/// up and `up` wrongly reports it ready. The 5ms cadence keeps the settle cheap for healthy pods.
const PORTLESS_WINDOW: Duration = Duration::from_millis(650);

/// How often a port/socket wait asks the world init whether its pod already exited.
const EXIT_PROBE_INTERVAL: Duration = Duration::from_millis(25);

/// Consecutive quiet tail polls that end the post-exit drain.
const SETTLE_QUIET_POLLS: u32 = 2;

/// Spacing between the two quiet-drain polls, so the last pumped pod-log lines land first.
const SETTLE_INTERVAL: Duration = Duration::from_millis(3);

/// Ceiling on the post-exit drain, so a half-written capture can never stall `up`.
const SETTLE_BUDGET: Duration = Duration::from_millis(100);

/// Hard cap on service lines mirrored into one run capture: a chatty server cannot bloat it.
const TAIL_MAX_LINES: usize = 200;

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

/// One readiness poll's typed sink, the service capture directory it tails, and the launch time.
pub(in crate::world) struct Progress {
    sink: BootSink,
    logs_dir: PathBuf,
    since_ms: u64,
}

impl Progress {
    pub(in crate::world) fn new(sink: BootSink, logs_dir: PathBuf, since_ms: u64) -> Self {
        Self {
            sink,
            logs_dir,
            since_ms,
        }
    }

    fn frame(&mut self, message: &str) {
        self.sink.progress(message);
    }

    /// Tops up the run capture (and live gutter) with one line the service itself printed.
    fn stream(&mut self, stream: Stream, text: &str) {
        self.sink.append_stream(stream, text);
    }
}

/// Incrementally mirrors one service's pod log into its run capture while the start is awaited.
///
/// Owns the [`Progress`] sink plus its tail cursor, so the two move in lockstep: a tail can never
/// outlive the step that streams it. Opened lazily: an immediate crash can land its marker before
/// the probe ever looks, and a missing or half-written index is simply "no bytes yet".
struct ServiceTail {
    progress: Progress,
    service: String,
    since_ms: u64,
    logs_dir: PathBuf,
    range: Option<LogRange>,
    lines: usize,
    capped: bool,
}

impl ServiceTail {
    fn new(progress: Progress, service: &str) -> Self {
        Self {
            logs_dir: progress.logs_dir.clone(),
            since_ms: progress.since_ms,
            progress,
            service: service.to_owned(),
            range: None,
            lines: 0,
            capped: false,
        }
    }

    /// Pumps any new pod-log lines into the capture; returns bytes read (`0` when quiet).
    fn drain(&mut self) -> usize {
        if self.capped {
            return 0;
        }
        if self.lines >= TAIL_MAX_LINES {
            self.cap_notice();
            return 0;
        }
        if self.range.is_none() {
            if !pod_log::log_path(&self.logs_dir, &self.service).is_file() {
                return 0;
            }
            match pod_log::read_range(&self.logs_dir, &self.service, Some(self.since_ms), None) {
                Ok(range) => self.range = Some(range),
                Err(_) => return 0,
            }
        }
        let Some(range) = self.range.as_mut() else {
            return 0;
        };
        let mut rows = Vec::new();
        let read = match range.read_lines(&mut |line| rows.push(line)) {
            Ok(read) => read,
            Err(_) => return 0,
        };
        for line in rows {
            if line.kind != LogKind::Line {
                continue;
            }
            if self.lines >= TAIL_MAX_LINES {
                self.cap_notice();
                break;
            }
            self.lines += 1;
            self.progress.stream(line.stream, &line.text);
        }
        read
    }

    /// Appends the one-time cap notice, then stops tailing for good.
    fn cap_notice(&mut self) {
        if self.capped {
            return;
        }
        self.capped = true;
        self.progress.stream(
            Stream::Stdout,
            &format!(
                "… capture capped at {TAIL_MAX_LINES} lines; run `slipfork logs {}` for the rest",
                self.service
            ),
        );
    }

    /// Drains the last pod-log lines after a settle signal, bounded by two quiet polls and 100ms.
    fn settle(&mut self) {
        let started = Instant::now();
        let mut quiet = 0_u32;
        while quiet < SETTLE_QUIET_POLLS && started.elapsed() < SETTLE_BUDGET {
            if self.drain() == 0 {
                quiet += 1;
            } else {
                quiet = 0;
            }
            std::thread::sleep(SETTLE_INTERVAL);
        }
    }

    /// Emits one frame per whole second spent waiting, never a per-2ms poll storm.
    fn report(&mut self, started: Instant, reported: &mut u32, subject: &str) {
        report_wait(&mut self.progress, started, reported, subject);
    }
}

impl Staged {
    /// Attaches the step-scoped boot sink so a multi-second wait can stream its own progress line.
    pub(in crate::world) fn attach_progress(
        &mut self,
        sink: BootSink,
        logs_dir: PathBuf,
        since_ms: u64,
    ) {
        self.progress = Some(Progress::new(sink, logs_dir, since_ms));
    }

    /// Finishes the node once its own readiness probe succeeds.
    pub(in crate::world) fn await_ready(self, pods_dir: &Path) -> Result<StepOutcome, BootFailure> {
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
            } => wait_unix(&socket, pods_dir, &label, &service, databases, progress)?,
            Ready::Portless { pods_dir, service } => {
                wait_portless(&pods_dir, &service, progress)?;
            }
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
    progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut reported = 0_u32;
    let mut tail = progress.map(|progress| ServiceTail::new(progress, service));
    let mut next_probe = started;
    loop {
        if let Ok(stream) = dial_tcp(pods_dir, port) {
            let _ = stream.shutdown(Shutdown::Both);
            if let Some(tail) = tail.as_mut() {
                tail.settle();
            }
            return Ok(());
        }
        if let Some(tail) = tail.as_mut() {
            tail.report(started, &mut reported, &format!("`{label}`"));
            tail.drain();
        }
        if Instant::now() >= next_probe {
            next_probe = Instant::now() + EXIT_PROBE_INTERVAL;
            if let Some(health) = pod_health(pods_dir, service) {
                if let Some(tail) = tail.as_mut() {
                    tail.settle();
                }
                return Err(exit_failure(service, health));
            }
        }
        if Instant::now() >= deadline {
            if let Some(tail) = tail.as_mut() {
                tail.settle();
            }
            return Err(BootFailure::with_remedy(
                format!("`{label}` never opened `:{port}` within 10s"),
                format!("run `slipfork logs {service}`"),
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_unix(
    socket: &Path,
    pods_dir: &Path,
    label: &str,
    service: &str,
    databases: Option<DatabaseBootstrap>,
    progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut reported = 0_u32;
    let mut last_fault = None;
    let mut tail = progress.map(|progress| ServiceTail::new(progress, service));
    let mut next_probe = started;
    loop {
        if UnixStream::connect(socket).is_ok() {
            // `connect` succeeds a few milliseconds before postgres accepts queries, so the
            // database reconcile is retried until the socket's own deadline instead of failing
            // the whole `fork` on `the database system is not yet accepting connections`.
            if let Some(databases) = &databases {
                match databases.ensure() {
                    Ok(()) => {
                        if let Some(tail) = tail.as_mut() {
                            tail.settle();
                        }
                        return Ok(());
                    }
                    Err(fault) => last_fault = Some(fault),
                }
            } else {
                if let Some(tail) = tail.as_mut() {
                    tail.settle();
                }
                return Ok(());
            }
        }
        if let Some(tail) = tail.as_mut() {
            tail.report(started, &mut reported, &format!("`{label}`"));
            tail.drain();
        }
        if Instant::now() >= next_probe {
            next_probe = Instant::now() + EXIT_PROBE_INTERVAL;
            if let Some(health) = pod_health(pods_dir, service) {
                if let Some(tail) = tail.as_mut() {
                    tail.settle();
                }
                return Err(exit_failure(service, health));
            }
        }
        if Instant::now() >= deadline {
            break;
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

fn wait_portless(
    pods_dir: &Path,
    service: &str,
    progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let deadline = Instant::now() + PORTLESS_WINDOW;
    let mut tail = progress.map(|progress| ServiceTail::new(progress, service));
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        // Drain every poll so a worker that dies mid-tick still leaves its output in `logs -r`.
        if let Some(tail) = tail.as_mut() {
            tail.drain();
        }
        if let Some(health) = pod_health(pods_dir, service) {
            if let Some(tail) = tail.as_mut() {
                tail.settle();
            }
            return match health {
                ServiceHealth::Exited(0) => Ok(()),
                other => Err(exit_failure(service, other)),
            };
        }
    }
    if let Some(tail) = tail.as_mut() {
        tail.settle();
    }
    Ok(())
}

/// One pod's health once the init has reaped it; `None` while it is still up or not listed yet.
fn pod_health(pods_dir: &Path, service: &str) -> Option<ServiceHealth> {
    let statuses = list_pods(pods_dir).ok()?;
    let status = statuses.iter().find(|status| status.id == service)?;
    if status.running && status.exit_code.is_none() {
        return None;
    }
    Some(classify_service(status.exit_code))
}

/// The one failure a premature pod exit produces, carrying its code only when it is known.
fn exit_failure(service: &str, health: ServiceHealth) -> BootFailure {
    match health {
        ServiceHealth::Exited(0) => BootFailure::with_remedy(
            format!("service `{service}` exited before it was ready"),
            format!("run `slipfork logs {service}`"),
        ),
        ServiceHealth::Exited(code) => BootFailure::with_remedy(
            format!("service `{service}` exited (exit {code})"),
            format!("run `slipfork logs {service}`"),
        )
        .with_exit_code(code),
        ServiceHealth::Running => BootFailure::with_remedy(
            format!("service `{service}` exited during boot"),
            format!("run `slipfork logs {service}`"),
        ),
    }
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
