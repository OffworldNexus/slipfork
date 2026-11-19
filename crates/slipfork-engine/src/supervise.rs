//! Kernel process identity and process-group supervision for one spawned child.
//!
//! A world component is launched as its own process-group leader (`CommandExt::process_group(0)`,
//! `pgid == the launcher's host pid`), so stop signals `kill(-pgid, …)` and reaches the `sh -eu -c`
//! wrapper, the service it forked, and any descendant that outlived the supervised child.
//! [`SupervisedProcess`] owns that one stop-and-reap path; [`ProcessIdentity`] pins the namespace
//! link and start ticks so a recycled host PID is never signalled.

#![forbid(unsafe_code)]

use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use slipfork_world::EngineError;

use crate::linux_cow::{self, ProcessSignal};
use crate::wait::{poll_until, wait_for_exit, wait_reaped};

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

/// One spawned supervisor child plus the kernel identity pinned at spawn time.
///
/// This is the engine's only stop path: `SIGTERM` the pinned identity (the whole group when it
/// leads one), wait out the caller's term grace, `SIGKILL` and reap the child, then confirm the
/// identity is gone. The caller keeps ownership of the child: on error it is still here and can be
/// restored into the service record so a retried teardown re-enters the same reaping path.
#[derive(Debug)]
pub(crate) struct SupervisedProcess {
    child: Option<Child>,
    identity: Option<ProcessIdentity>,
}

impl SupervisedProcess {
    pub(crate) fn from_parts(child: Option<Child>, identity: Option<ProcessIdentity>) -> Self {
        Self { child, identity }
    }

    /// Give the child back so a failed stop can restore it into the supervision record.
    pub(crate) fn into_child(self) -> Option<Child> {
        self.child
    }

    /// The supervised child, for a caller that must kill it before reading its stderr tail.
    pub(crate) fn child_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    /// Close the child's `stdin` lifeline (the world PID 1's voluntary-exit signal).
    pub(crate) fn close_stdin(&mut self) {
        if let Some(child) = self.child.as_mut() {
            drop(child.stdin.take());
        }
    }

    /// Wait (bounded) for the supervised child to exit, reaping it.
    pub(crate) fn wait_exit(&mut self, grace: Duration) -> bool {
        self.child
            .as_mut()
            .is_none_or(|child| wait_for_exit(child, grace))
    }

    /// `SIGKILL` and reap the supervised child within `grace`.
    pub(crate) fn reap_child(&mut self, grace: Duration, label: &str) -> Result<(), EngineError> {
        match self.child.as_mut() {
            Some(child) => kill_and_reap_child(child, grace, label),
            None => Ok(()),
        }
    }

    /// The one `SIGTERM` → `SIGKILL` → reap → confirm-gone path for one supervised process tree.
    ///
    /// `d_state_grace` is the caller's `D`-state retry budget: a service group retries for
    /// [`crate::wait::D_STATE_KILL_GRACE`], while the world PID 1 keeps its existing kill grace and
    /// reports at once. [`crate::wait::wait_reaped`] names the surviving pid, its `/proc` state,
    /// and the **measured** time since the first `SIGTERM` (the configured kill and `D`-state
    /// budgets sit beside it).
    pub(crate) fn stop(
        &mut self,
        term_grace: Duration,
        kill_grace: Duration,
        d_state_grace: Duration,
        label: &str,
    ) -> Result<(), EngineError> {
        let term_started = Instant::now();
        let deadline = term_started + term_grace;
        if let Some(identity) = self.identity.as_ref() {
            let _ = identity.signal(ProcessSignal::Terminate);
        }
        let mut stopped = true;
        if let Some(child) = self.child.as_mut() {
            stopped = wait_for_exit(child, term_grace);
        }
        if let Some(identity) = self.identity.as_ref() {
            stopped =
                identity.wait_gone(deadline.saturating_duration_since(Instant::now())) && stopped;
        }
        if !stopped {
            if let Some(identity) = self.identity.as_ref()
                && let Err(err) = identity.signal(ProcessSignal::Kill)
                && (err.raw_os_error() != Some(libc::ESRCH) || identity.is_live())
            {
                return Err(EngineError::Execution(format!(
                    "cannot SIGKILL {label} (pid {}): {err}",
                    identity.pid
                )));
            }
            if let Some(child) = self.child.as_mut() {
                kill_and_reap_child(child, kill_grace, label)?;
            }
        }
        match self.identity.as_ref() {
            Some(identity) => wait_reaped(identity, term_started, kill_grace, d_state_grace, label),
            None => Ok(()),
        }
    }
}

/// Host pids of `pid`'s direct children, read from `/proc/<pid>/task/<pid>/children`.
pub(crate) fn host_child_pids(pid: u32) -> io::Result<Vec<u32>> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))?;
    Ok(raw
        .split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect())
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

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::wait::{poll_until, wait_reaped};

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
