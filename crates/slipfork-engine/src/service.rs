//! World-scoped, size-capped service log retention (`M2.7`) plus the pure process-supervision
//! helpers every world engine shares.
//!
//! A [`ServiceLogs`] handle is minted by an engine when a service starts and stays readable after
//! [`crate::WorldEngine::stop_service`] or a failed hook — failed worlds must remain inspectable
//! (`ADR-0008` §4). Output is captured per service, never per call, so no consumer can grow memory
//! without bound: the retained window keeps the newest [`SERVICE_LOG_CAP_BYTES`] bytes.
//!
//! The supervision helpers below are engine-independent: they build the `sh -c 'cd … && exec …'`
//! wrapper, name a `crun` container, poll `/proc` for a bounded teardown, stream a child's output,
//! and translate a host mount into the guest path the world sees. They carry no engine state, so
//! every adapter shares one implementation instead of forking its own copy.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::linux_cow::{self, ProcessSignal};
use crate::rootfs::{BackingServiceKey, OciRootfs};
use crate::{EngineError, ExecOutput, StreamChunk, StreamSource, WorldMount};

/// Bytes of service output retained per service (oldest chunks are evicted first).
pub const SERVICE_LOG_CAP_BYTES: usize = 64 * 1024;

/// Poll interval for every bounded wait (`/proc` disappearance, child reaping, readiness probes).
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Size-capped retained output of one world-owned service.
///
/// Cloning shares the same retained window; every clone observes all chunks captured from that
/// point on.
///
/// # Poisoning
///
/// A panic in an engine thread must never erase a failed service's output. The retained window has
/// no cross-field invariant a panic can corrupt, so every lock acquisition recovers the guard
/// instead of discarding the data; [`ServiceLogs::poisoned`] reports the poisoning as a diagnostic
/// for callers that want to surface it.
#[derive(Clone)]
pub struct ServiceLogs {
    inner: Arc<LogBuffer>,
}

impl ServiceLogs {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(LogBuffer::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LogState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Every retained chunk, oldest first, in capture order.
    pub fn snapshot(&self) -> Vec<StreamChunk> {
        self.lock().chunks.iter().cloned().collect()
    }

    /// `true` when a panic poisoned the retained window.
    ///
    /// Output is still delivered by [`ServiceLogs::snapshot`] and [`ServiceLogs::follow`] — this is
    /// a diagnostic, never a reason to report empty logs.
    pub fn poisoned(&self) -> bool {
        self.inner.state.is_poisoned()
    }

    /// Replay every retained chunk, then deliver new chunks as they are captured until the service
    /// stops or exits.
    ///
    /// Returns once the log is closed — i.e. after [`crate::WorldEngine::stop_service`], the
    /// service exiting on its own, or the parent world being destroyed.
    pub fn follow(&self, on_chunk: &mut dyn FnMut(StreamChunk)) {
        let mut state = self.lock();
        let mut cursor = 0_u64;
        loop {
            let base = state.appended - state.chunks.len() as u64;
            if cursor < base {
                // The retained window evicted chunks this follower never saw; skip what is gone.
                cursor = base;
            }
            if cursor < state.appended {
                let index = (cursor - base) as usize;
                let chunk = state.chunks[index].clone();
                cursor += 1;
                drop(state);
                on_chunk(chunk);
                state = self.lock();
                continue;
            }
            if state.closed {
                return;
            }
            state = match self.inner.condvar.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// `true` once no further chunks can be captured (service stopped, exited, or world gone).
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    pub(crate) fn push(&self, source: StreamSource, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.inner.push(StreamChunk { source, bytes });
    }

    pub(crate) fn close(&self) {
        self.inner.close();
    }
}

impl std::fmt::Debug for ServiceLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("ServiceLogs")
            .field("chunks", &state.chunks.len())
            .field("bytes", &state.bytes)
            .field("closed", &state.closed)
            .field("poisoned", &self.poisoned())
            .finish()
    }
}

#[derive(Default)]
struct LogBuffer {
    state: Mutex<LogState>,
    condvar: Condvar,
}

#[derive(Default)]
struct LogState {
    chunks: VecDeque<StreamChunk>,
    appended: u64,
    bytes: usize,
    closed: bool,
}

impl LogBuffer {
    fn push(&self, chunk: StreamChunk) {
        // Writer side also recovers a poisoned lock: captured output must never be dropped.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return;
        }
        state.appended += 1;
        state.bytes += chunk.bytes.len();
        state.chunks.push_back(chunk);
        while state.bytes > SERVICE_LOG_CAP_BYTES && state.chunks.len() > 1 {
            if let Some(dropped) = state.chunks.pop_front() {
                state.bytes -= dropped.bytes.len();
            }
        }
        drop(state);
        self.condvar.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        drop(state);
        self.condvar.notify_all();
    }
}

/// `sh -c 'cd -- "$1" && shift && exec "$@"' sh <cwd> [argv...]`.
///
/// The `cd` runs **inside** the entered mount namespace, so `getcwd(2)` resolves through the
/// world's bind mounts instead of the hidden host path.
pub(crate) fn world_cwd_wrapper(cwd: &Path, argv: &[String]) -> Vec<String> {
    let mut wrapped = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "cd -- \"$1\" && shift && exec \"$@\"".to_owned(),
        "sh".to_owned(),
        cwd.display().to_string(),
    ];
    wrapped.extend(argv.iter().cloned());
    wrapped
}

/// Stable `crun` container id derived from the world sequence and service id.
pub(crate) fn container_id(world_seq: u64, service_id: &str) -> String {
    let slug: String = service_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "service" } else { slug };
    format!("sf{world_seq}-{}", &slug[..slug.len().min(48)])
}

/// Host pids of `pid`'s direct children, read from `/proc/<pid>/task/<pid>/children`.
pub(crate) fn host_child_pids(pid: u32) -> io::Result<Vec<u32>> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    Ok(raw
        .split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect())
}

/// Poll `step` every [`POLL_INTERVAL`] until it is ready or `grace` elapses.
///
/// The one bounded-wait loop: `step` returns `Some(..)` to stop with a value, `None` to keep
/// polling. A caller that must stop early returns its terminal value from `step` (for example
/// [`wait_for_exit`]'s `Some(false)` on `try_wait` error).
pub(crate) fn poll_until<T>(grace: Duration, mut step: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + grace;
    loop {
        if let Some(value) = step() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Wait (bounded) for `supervisor_pid`'s first child — the process `nsenter --pid` forked into the
/// world's PID namespace.
pub(crate) fn wait_for_inner_pid(supervisor_pid: u32, grace: Duration) -> Option<u32> {
    poll_until(grace, || {
        host_child_pids(supervisor_pid)
            .ok()
            .and_then(|children| children.first().copied())
    })
}

/// Wait (bounded) for `child` to exit, reaping it.
pub(crate) fn wait_for_exit(child: &mut Child, grace: Duration) -> bool {
    poll_until(grace, || match child.try_wait() {
        Ok(Some(_)) => Some(true),
        Ok(None) => None,
        Err(_) => Some(false),
    }) == Some(true)
}

/// Kernel identity of one spawned process, pinning its `/proc/<pid>/ns/pid` namespace link and
/// `/proc/<pid>/stat` start ticks so a recycled host PID is never signalled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) pid_ns: PathBuf,
    pub(crate) start_ticks: u64,
}

impl ProcessIdentity {
    pub(crate) fn capture(pid: u32) -> Option<Self> {
        if pid <= 1 {
            return None;
        }
        let pid_ns = std::fs::read_link(format!("/proc/{pid}/ns/pid")).ok()?;
        let start_ticks = proc_start_ticks(pid)?;
        Some(Self {
            pid,
            pid_ns,
            start_ticks,
        })
    }

    pub(crate) fn matches_live(&self) -> bool {
        std::fs::read_link(format!("/proc/{}/ns/pid", self.pid))
            .ok()
            .as_deref()
            == Some(self.pid_ns.as_path())
            && proc_start_ticks(self.pid) == Some(self.start_ticks)
    }

    pub(crate) fn signal(&self, signal: ProcessSignal) -> io::Result<()> {
        if !self.matches_live() {
            return Ok(());
        }
        linux_cow::signal_process(self.pid, signal)
    }

    pub(crate) fn wait_gone(&self, grace: Duration) -> bool {
        poll_until(grace, || (!self.matches_live()).then_some(())).is_some()
    }
}

/// Field 22 (`starttime`) of `/proc/<pid>/stat`, parsed past the final `)` of `comm`.
fn proc_start_ticks(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = raw.get(raw.rfind(')')? + 1..)?;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// `SIGKILL` `child` (if still running) and reap it within `grace`, surfacing any `try_wait`,
/// `kill`, or timeout failure as a typed [`EngineError::Execution`] naming `label`.
pub(crate) fn kill_and_reap_child(
    child: &mut Child,
    grace: Duration,
    label: &str,
) -> Result<(), EngineError> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(err) => {
            return Err(EngineError::Execution(format!(
                "cannot observe {label}: {err}"
            )));
        }
    }
    if let Err(err) = child.kill() {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                return Err(EngineError::Execution(format!(
                    "cannot SIGKILL {label}: {err}"
                )));
            }
            Err(wait_err) => {
                return Err(EngineError::Execution(format!(
                    "cannot SIGKILL {label} ({err}); try_wait failed: {wait_err}"
                )));
            }
        }
    }
    if !wait_for_exit(child, grace) {
        return Err(EngineError::Execution(format!(
            "{label} did not exit within {}s of SIGKILL",
            grace.as_secs()
        )));
    }
    Ok(())
}

/// Run `command` to completion, streaming interleaved stdout/stderr chunks to `on_output`.
pub(crate) fn run_streamed(
    mut command: Command,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    let mut child = command
        .spawn()
        .map_err(|err| EngineError::Execution(format!("cannot execute: {err}")))?;
    let (sender, receiver) = mpsc::channel::<StreamChunk>();
    let mut readers = Vec::with_capacity(2);
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(stdout, StreamSource::Stdout, sender.clone()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(stderr, StreamSource::Stderr, sender.clone()));
    }
    drop(sender);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for chunk in receiver {
        match chunk.source {
            StreamSource::Stdout => stdout.extend_from_slice(&chunk.bytes),
            StreamSource::Stderr => stderr.extend_from_slice(&chunk.bytes),
        }
        on_output(chunk);
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child
        .wait()
        .map_err(|err| EngineError::Execution(err.to_string()))?;
    Ok(ExecOutput {
        exit_code: status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Read `source` to EOF in 4 KiB chunks, handing each chunk to `sink`; stop when `sink` returns
/// `false` (a closed channel or a closed log window).
fn pump<R: Read>(mut source: R, mut sink: impl FnMut(Vec<u8>) -> bool) {
    let mut buffer = [0_u8; 4096];
    while let Ok(read) = source.read(&mut buffer) {
        if read == 0 {
            break;
        }
        if !sink(buffer[..read].to_vec()) {
            break;
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    source: R,
    kind: StreamSource,
    sender: mpsc::Sender<StreamChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        pump(source, |bytes| {
            sender
                .send(StreamChunk {
                    source: kind,
                    bytes,
                })
                .is_ok()
        })
    })
}

/// Capture one service stream into its retained [`ServiceLogs`] window.
pub(crate) fn spawn_log_reader<R: Read + Send + 'static>(
    source: R,
    kind: StreamSource,
    log: ServiceLogs,
) {
    thread::spawn(move || {
        pump(source, |bytes| {
            log.push(kind, bytes);
            true
        })
    });
}

/// Best-effort helper stderr tail for actionable boot diagnostics.
pub(crate) fn drain_stderr(child: &mut Child) -> String {
    let mut raw = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut raw);
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 2000 {
        trimmed.chars().take(2000).collect()
    } else {
        trimmed.to_owned()
    }
}

/// Refuse a container mount destination the backing-service image does not carry.
pub(crate) fn ensure_container_destination(
    image: &OciRootfs,
    image_key: &BackingServiceKey,
    container_path: &Path,
) -> Result<(), EngineError> {
    let relative = container_path
        .strip_prefix("/")
        .map_err(|_| EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' must be absolute",
                container_path.display()
            ),
        })?;
    if !image.rootfs.join(relative).exists() {
        return Err(EngineError::InvalidWorldSpec {
            reason: format!(
                "container path '{}' does not exist in backing-service image {image_key}; \
                 the image rootfs must carry the mount destination",
                container_path.display()
            ),
        });
    }
    Ok(())
}

/// Translate one host path into the guest path the world sees after `pivot_root`.
pub(crate) fn world_guest_for_host(mounts: &[WorldMount], host_path: &Path) -> Option<PathBuf> {
    mounts.iter().find_map(|mount| {
        host_path
            .strip_prefix(&mount.host_path)
            .ok()
            .map(|relative| mount.guest_path.join(relative))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_log_still_serves_captured_output_and_reports_the_diagnostic() {
        let logs = ServiceLogs::new();
        logs.push(StreamSource::Stdout, b"before-panic\n".to_vec());
        let poisoner = logs.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.inner.state.lock().expect("log lock");
            panic!("simulate an engine-thread panic while holding the log lock");
        })
        .join();
        assert!(logs.poisoned(), "the poisoning must be observable");
        // The retained window is never silently reported as empty.
        assert_eq!(logs.snapshot().len(), 1);
        assert_eq!(
            String::from_utf8_lossy(&logs.snapshot()[0].bytes),
            "before-panic\n"
        );
        // Capture keeps working after a poison, and follow() drains it.
        logs.push(StreamSource::Stderr, b"after-panic\n".to_vec());
        assert_eq!(logs.snapshot().len(), 2);
        logs.close();
        let mut followed = Vec::new();
        logs.follow(&mut |chunk| followed.push(chunk));
        assert_eq!(followed.len(), 2, "follow must replay the retained window");
    }
}
