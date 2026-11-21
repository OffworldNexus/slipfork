//! Readiness probes for one staged boot node: service ports and PostgreSQL unix sockets.
//!
//! Each probe is returned as a [`Ready`] value, so the caller awaits it after the runtime lock is
//! dropped and a slow database never serialises independent nodes. The wait streams one progress
//! frame per second through the [`Progress`] handle attached by [`Staged::attach_progress`].

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use slipfork_world::control::dial_tcp;
use slipfork_world::{BootFailure, BootState, StepOutcome};

use crate::world::supervise::BootSink;

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
    },
}

/// One readiness poll's typed sink plus the step it must stay pinned to.
pub(in crate::world) struct Progress {
    sink: BootSink,
    step: usize,
    total: usize,
    label: String,
}

impl Progress {
    pub(in crate::world) fn new(sink: BootSink, step: usize, total: usize, label: &str) -> Self {
        Self {
            sink,
            step,
            total,
            label: label.to_owned(),
        }
    }

    fn frame(&mut self, message: &str) {
        self.sink
            .progress(self.step, self.total, &self.label, message);
    }
}

impl Staged {
    /// Attaches the boot sink so a multi-second wait can stream its own progress line.
    pub(in crate::world) fn attach_progress(
        &mut self,
        sink: BootSink,
        step: usize,
        total: usize,
        label: &str,
    ) {
        self.progress = Some(Progress::new(sink, step, total, label));
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
            } => wait_unix(&socket, &label, &service, progress)?,
        }
        Ok(StepOutcome {
            state: self.state,
            detail: self.detail,
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
    mut progress: Option<Progress>,
) -> Result<(), BootFailure> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut reported = 0_u32;
    while Instant::now() < deadline {
        if UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        if let Some(progress) = progress.as_mut() {
            report_wait(progress, started, &mut reported, &format!("`{label}`"));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Err(BootFailure::with_remedy(
        format!("`{label}` never opened `{}` within 10s", socket.display()),
        format!("run `slipfork logs {service}`"),
    ))
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
