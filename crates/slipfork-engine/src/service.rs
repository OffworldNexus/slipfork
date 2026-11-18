//! World-scoped, size-capped service log retention (`M2.7`) plus the pure process-supervision
//! helpers every world engine shares.
//!
//! A [`ServiceLogs`] handle is minted by an engine when a service starts and stays readable after
//! [`slipfork_world::WorldEngine::stop_service`] or a failed hook — failed worlds must remain inspectable
//! (`ADR-0008` §4). Output is captured per service, never per call, so no consumer can grow memory
//! without bound: the retained window keeps the newest [`SERVICE_LOG_CAP_BYTES`] bytes.
//!
//! The supervision helpers below are engine-independent: they build the `sh -c 'cd … && exec …'`
//! wrapper, name a `crun` container, poll `/proc` for a bounded teardown, stream a child's output,
//! and translate a host mount into the guest path the world sees. They carry no engine state, so
//! every adapter shares one implementation instead of forking its own copy.

#![forbid(unsafe_code)]

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::linux_cow::{self, ProcessSignal};
use slipfork_world::rootfs::{BackingServiceKey, OciRootfs};
use slipfork_world::{EngineError, ExecOutput, ServiceLogs, StreamChunk, StreamSource, WorldMount};

/// Bytes of service output retained per service (oldest chunks are evicted first).
pub const SERVICE_LOG_CAP_BYTES: usize = 64 * 1024;

/// Poll interval for every bounded wait (`/proc` disappearance, child reaping, readiness probes).
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Upper bound on retrying a `SIGKILL`ed process that is stuck in uninterruptible sleep (`D`).
///
/// `SIGKILL` is only delivered when the task leaves the kernel wait it is blocked in. A `D`-state
/// task is therefore not unkillable, merely slow to observe the signal: the stop path keeps
/// retrying with exponential backoff for this window instead of declaring it unkillable after the
/// bare per-signal grace.
pub(crate) const D_STATE_KILL_GRACE: Duration = Duration::from_secs(60);

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

/// The process group one spawned world process owns.
///
/// A component is launched as `nsenter … sh -eu -c <run_command>`: the `sh` and the real service
/// are **descendants** of the launcher, so stopping only the first child would orphan the process
/// that owns the port. Every world process therefore becomes its own process-group leader
/// (`CommandExt::process_group(0)`, pgid == the launcher's host pid) and stop signals
/// `kill(-pgid, …)`, which reaches the whole tree including a descendant that outlives the
/// supervised child.
///
/// `world_pid_ns` is the world's `/proc/<init>/ns/pid` link: group membership survives the PID
/// namespace switch (it is one kernel group whose leader is only visible from the parent
/// namespace), so it is the discriminator that proves a surviving member belongs to this world
/// and not to a recycled pgid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessGroup {
    pub(crate) pgid: u32,
    pub(crate) world_pid_ns: PathBuf,
}

/// Kernel identity of one spawned process, pinning its `/proc/<pid>/ns/pid` namespace link and
/// `/proc/<pid>/stat` start ticks so a recycled host PID is never signalled.
///
/// A world process's supervisor additionally carries the [`ProcessGroup`] it leads: signalling the
/// identity then signals the group, and liveness includes a group member that outlived the leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) pid_ns: PathBuf,
    pub(crate) start_ticks: u64,
    pub(crate) group: Option<ProcessGroup>,
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
            group: None,
        })
    }

    /// Capture `pid` as the leader of its own process group inside `world_pid_ns`.
    ///
    /// Fails closed (`None`) when the process is not the leader of group `pid`, so a spawn that
    /// could not create its group is never recorded as one that can be group-signalled.
    pub(crate) fn capture_group_leader(pid: u32, world_pid_ns: PathBuf) -> Option<Self> {
        let mut identity = Self::capture(pid)?;
        if proc_pgrp(pid) != Some(pid) {
            return None;
        }
        identity.group = Some(ProcessGroup {
            pgid: pid,
            world_pid_ns,
        });
        Some(identity)
    }

    /// `true` only while this exact process is alive **and** can still make progress.
    ///
    /// An unreaped zombie keeps its `/proc/<pid>/ns/pid` link and start ticks, so the namespace +
    /// ticks pin alone called a corpse live: it cannot be signalled usefully and waits must see it
    /// gone, so every caller treats `Z`/`X` as dead.
    pub(crate) fn matches_live(&self) -> bool {
        std::fs::read_link(format!("/proc/{}/ns/pid", self.pid))
            .ok()
            .as_deref()
            == Some(self.pid_ns.as_path())
            && proc_start_ticks(self.pid) == Some(self.start_ticks)
            && proc_state(self.pid).is_none_or(state_is_live)
    }

    /// `true` while the pinned process itself is still live, ignoring any group member.
    pub(crate) fn is_live_leader(&self) -> bool {
        self.matches_live()
    }

    /// `true` while the process — or, for a group identity, any member of its group — lives.
    pub(crate) fn is_live(&self) -> bool {
        self.is_live_leader()
            || self
                .group
                .as_ref()
                .is_some_and(|group| !group_member_pids(group).is_empty())
    }

    /// Signal the pinned process, or the whole group when this identity leads one.
    ///
    /// A single-pid identity is checked and then signalled, so a recycled PID is never signalled.
    /// A group identity is checked by scanning its members ([`group_member_pids`]) and then
    /// signalled with `kill(-pgid, …)`; that scan-then-signal is **not atomic**, so a group that
    /// disappears between the two can in principle have its pgid recycled and receive the signal.
    /// The engine closes that window for the common case by dropping the group handle after a
    /// successful stop (a repeated stop then has nothing to signal), and every member is
    /// re-verified against the world PID namespace before the group is addressed.
    pub(crate) fn signal(&self, signal: ProcessSignal) -> io::Result<()> {
        match &self.group {
            Some(group) => {
                if !self.is_live() {
                    return Ok(());
                }
                linux_cow::signal_process_group(group.pgid, signal)
            }
            None => {
                if !self.matches_live() {
                    return Ok(());
                }
                linux_cow::signal_process(self.pid, signal)
            }
        }
    }

    pub(crate) fn wait_gone(&self, grace: Duration) -> bool {
        poll_until(grace, || (!self.is_live()).then_some(())).is_some()
    }

    /// The still-live pid and its `/proc/<pid>/stat` state character, for a truthful stop error.
    pub(crate) fn survivor(&self) -> Option<(u32, char)> {
        match &self.group {
            Some(group) => group_member_pids(group)
                .into_iter()
                .find_map(|pid| proc_state(pid).map(|state| (pid, state)))
                .or_else(|| {
                    self.matches_live()
                        .then(|| proc_state(self.pid).map(|state| (self.pid, state)))
                        .flatten()
                }),
            None => self
                .matches_live()
                .then(|| proc_state(self.pid).map(|state| (self.pid, state)))
                .flatten(),
        }
    }
}

/// Wait for `identity` to disappear, retrying a `D`-state survivor with exponential backoff.
///
/// A task in uninterruptible sleep has not acted on `SIGKILL` yet but is not unkillable; once the
/// kernel wait completes the pending signal is delivered. Only a survivor that is *not* in `D`
/// after `grace`, or a `D`-state survivor that outlasts `d_state_grace`, is reported.
///
/// `d_state_grace` is the caller's retry budget ([`D_STATE_KILL_GRACE`] for a service group,
/// `Duration::ZERO` for a caller that must report at once and keeps its existing grace policy).
/// `term_started` is when the caller sent its first `SIGTERM`: the error reports the **measured**
/// elapsed time since then, which is the only honest number once a `D`-state survivor has outlived
/// both configured budgets.
pub(crate) fn wait_reaped(
    identity: &ProcessIdentity,
    term_started: Instant,
    grace: Duration,
    d_state_grace: Duration,
    label: &str,
) -> Result<(), EngineError> {
    wait_reaped_observing(term_started, grace, d_state_grace, label, || {
        identity.survivor()
    })
}

/// The retry policy behind [`wait_reaped`], over a caller-supplied survivor observation.
///
/// `survivor` returns the still-live pid and its `/proc` state, or `None` once nothing is left.
/// Production always passes [`ProcessIdentity::survivor`]; the seam exists so the `D`-state
/// retry can be unit-tested with a deterministic state source.
pub(crate) fn wait_reaped_observing(
    term_started: Instant,
    grace: Duration,
    d_state_grace: Duration,
    label: &str,
    mut survivor: impl FnMut() -> Option<(u32, char)>,
) -> Result<(), EngineError> {
    if poll_until(grace, || survivor().is_none().then_some(())).is_some() {
        return Ok(());
    }
    let deadline = Instant::now() + d_state_grace;
    let mut backoff = POLL_INTERVAL;
    loop {
        let Some((pid, state)) = survivor() else {
            return Ok(());
        };
        if state != 'D' || Instant::now() >= deadline {
            return Err(EngineError::Execution(format!(
                "{label} survived SIGTERM and SIGKILL for {:?} (kill grace {grace:?}, D-state \
                 grace {d_state_grace:?}): pid {pid} is still in /proc state '{state}'",
                term_started.elapsed()
            )));
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(1));
    }
}

/// Live members of `group` that belong to its world PID namespace.
fn group_member_pids(group: &ProcessGroup) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| proc_pgrp(*pid) == Some(group.pgid))
        .filter(|pid| {
            std::fs::read_link(format!("/proc/{pid}/ns/pid"))
                .ok()
                .as_deref()
                == Some(group.world_pid_ns.as_path())
        })
        .collect()
}

/// Field 5 (`pgrp`) of `/proc/<pid>/stat`, parsed past the final `)` of `comm`.
fn proc_pgrp(pid: u32) -> Option<u32> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = raw.get(raw.rfind(')')? + 1..)?;
    after_comm.split_whitespace().nth(2)?.parse().ok()
}

/// Field 3 (`state`) of `/proc/<pid>/stat`, parsed past the final `)` of `comm`.
fn proc_state(pid: u32) -> Option<char> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.get(raw.rfind(')')? + 1..)?
        .split_whitespace()
        .next()?
        .chars()
        .next()
}

/// `true` for a `/proc/<pid>/stat` state that can still run: the zombie (`Z`) and dead (`X`)
/// states have already exited and only keep their pid until the parent reaps them.
fn state_is_live(state: char) -> bool {
    state != 'Z' && state != 'X'
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
    command: Command,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    run_streamed_inner(command, None, on_output)
}

/// Run `command` with `input` copied to its stdin while streaming interleaved stdout/stderr.
pub(crate) fn run_streamed_with_input(
    command: Command,
    input: Box<dyn Read + Send>,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    run_streamed_inner(command, Some(input), on_output)
}

fn run_streamed_inner(
    mut command: Command,
    input: Option<Box<dyn Read + Send>>,
    on_output: &mut dyn FnMut(StreamChunk),
) -> Result<ExecOutput, EngineError> {
    let mut child = command
        .spawn()
        .map_err(|err| EngineError::Execution(format!("cannot execute: {err}")))?;
    let writer = match (input, child.stdin.take()) {
        (Some(mut input), Some(mut stdin)) => Some(thread::spawn(move || {
            let copied = io::copy(&mut input, &mut stdin);
            drop(stdin);
            copied
        })),
        (None, _) => None,
        (Some(_), None) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Execution(
                "cannot stream input: the child has no piped stdin".to_owned(),
            ));
        }
    };
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
    if let Some(writer) = writer {
        match writer.join() {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                return Err(EngineError::Execution(format!(
                    "streamed input failed: {error}"
                )));
            }
            Err(_) => {
                return Err(EngineError::Execution(
                    "streamed input writer panicked".to_owned(),
                ));
            }
        }
    }
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
            .map(|relative| {
                // `join("")` would append a trailing separator; the mount root maps to itself.
                if relative.as_os_str().is_empty() {
                    mount.guest_path.clone()
                } else {
                    mount.guest_path.join(relative)
                }
            })
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn wait_reaped_names_the_surviving_pid_and_its_proc_state() {
        // A real live process: no signal is sent here, only the bounded wait, so the survivor is
        // reported with the pid the kernel actually shows and its `/proc/<pid>/stat` state.
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn a survivor");
        let identity = ProcessIdentity::capture(child.id()).expect("capture the survivor identity");
        let err = wait_reaped(
            &identity,
            Instant::now(),
            Duration::from_millis(20),
            Duration::ZERO,
            "unit service 'api'",
        )
        .expect_err("a live survivor must be reported");
        let message = err.to_string();
        assert!(
            message.contains(&format!("pid {}", identity.pid)),
            "the stop error must name the surviving pid: {message}"
        );
        assert!(
            message.contains("/proc state 'S'") || message.contains("/proc state 'R'"),
            "the stop error must name the survivor's /proc state: {message}"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_d_state_survivor_is_retried_past_the_nominal_grace_instead_of_being_reported() {
        // Deterministic state source: the survivor stays in `D` well past the nominal grace and
        // only then disappears. With a zero nominal grace the only way this can succeed is the
        // `D`-state retry loop; a non-`D` survivor would have been reported on the first probe.
        let mut probes = 0_u32;
        let result = wait_reaped_observing(
            Instant::now(),
            Duration::ZERO,
            Duration::from_secs(5),
            "unit service 'api'",
            || {
                probes += 1;
                (probes < 6).then_some((4242, 'D'))
            },
        );
        assert!(
            result.is_ok(),
            "the D-state survivor must be retried: {result:?}"
        );
        assert_eq!(probes, 6, "every retry must probe the survivor again");
    }

    #[test]
    fn a_d_state_survivor_that_outlasts_its_budget_is_reported_with_its_state() {
        // The survivor burned both budgets: the report must name the *measured* elapsed time since
        // the first `SIGTERM`, not the configured grace, which is what used to make a ~70 s stop
        // read "survived … for 10s". Both budgets are zero here so the probe is instant; the
        // measurement still comes from the caller's first-`SIGTERM` instant, not from a budget.
        let err = wait_reaped_observing(
            Instant::now() - Duration::from_secs(70),
            Duration::ZERO,
            Duration::ZERO,
            "unit service 'api'",
            || Some((4242, 'D')),
        )
        .expect_err("a D-state survivor past its budget must be reported");
        let message = err.to_string();
        assert!(message.contains("pid 4242"), "{message}");
        assert!(message.contains("/proc state 'D'"), "{message}");
        assert!(
            message.contains("for 70."),
            "the report must measure the time since the first SIGTERM: {message}"
        );
        assert!(
            message.contains("kill grace"),
            "the configured deadline stays visible next to the measurement: {message}"
        );
    }

    #[test]
    fn only_a_running_proc_state_is_live() {
        // The state-char mapping is the whole zombie fix: `Z` (zombie) and `X` (dead) keep their
        // pid but have exited, every other state can still act on a signal or finish on its own.
        for (state, live) in [
            ('R', true),
            ('S', true),
            ('D', true),
            ('T', true),
            ('Z', false),
            ('X', false),
        ] {
            assert_eq!(state_is_live(state), live, "/proc state '{state}'");
        }
    }

    #[test]
    fn an_unreaped_zombie_is_not_live() {
        // A child that exits is kept in `/proc` as a zombie until its parent reaps it, and it
        // keeps the very pid-namespace link and start ticks the identity pins — the hole that let
        // `world_is_live` call a world live after its PID 1 had exited. The identity is captured
        // while the child runs; it is then only observed, never `wait`ed, until the kernel reports
        // the zombie state, so the assertion below cannot pass for a live process.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.2"])
            .spawn()
            .expect("spawn a child");
        let identity = ProcessIdentity::capture(child.id()).expect("capture the live identity");
        let zombie = poll_until(Duration::from_secs(5), || {
            (proc_state(identity.pid) == Some('Z')).then_some(())
        });
        assert!(
            zombie.is_some(),
            "pid {} must be observable as an unreaped zombie",
            identity.pid
        );
        assert!(
            !identity.matches_live(),
            "an unreaped zombie must not read as live"
        );
        assert!(
            identity.wait_gone(Duration::from_millis(50)),
            "a zombie is gone for every wait"
        );
        let _ = child.wait();
    }
}
