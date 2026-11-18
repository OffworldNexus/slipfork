//! The version-agnostic `daemon.sock` handover that runs before `daemon install` enables units.
//!
//! `slipfork daemon install` must never let an older (or zero-setup) daemon keep the control
//! socket while systemd starts the installed one (`ADR-0011` §3). [`release_daemon`] runs one
//! order: stop the units this installer writes, ask a daemon that speaks the typed contract to shut
//! down and await it, then prove any remaining holder — peer pid from `SO_PEERCRED`, executable
//! basename from `/proc/<pid>/exe`, pinned with a `pidfd` — before a signal exists to send. A
//! holder that cannot be proven is refused in one line and never signalled.
//!
//! Every probe and the shutdown go through the injected [`UnitInstaller`], so one adapter sees one
//! socket at a time and the install sequence only decides the order.

#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::daemon_unit_install::UnitInstaller;
use slipfork_edge::{DaemonControl, DaemonControlClient, DaemonControlServer};
use slipfork_engine::linux_cow::{ProcessSignal, open_pidfd, peer_pid, signal_pidfd};

/// One daemon-socket budget shared by the handover and the CLI's auto-spawn wait.
pub const DAEMON_GRACE: Duration = Duration::from_secs(15);

/// Poll interval while waiting for `<runtime_dir>/daemon.sock` to answer or disappear.
pub const DAEMON_POLL: Duration = Duration::from_millis(50);

/// The handover's bounded-wait budget: one grace window plus the probe interval.
///
/// `daemon install` keeps time with plain [`std::time::Instant`] observations, and the production caller
/// passes [`DAEMON_HANDOVER`]; a test passes a zero budget and reaches the refusal immediately.
#[derive(Debug, Clone, Copy)]
pub struct HandoverBudget {
    /// How long a held socket may be awaited before the refusal.
    pub grace: Duration,
    /// Interval between probes while waiting.
    pub poll: Duration,
}

/// The systemd units `daemon install` enables together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedUnit {
    /// `slipforkd.socket` (owns the privileged binds).
    Socket,
    /// `slipforkd.service` (the unprivileged daemon).
    Service,
}

impl ManagedUnit {
    /// The unit file name `systemctl enable --now` receives.
    pub fn unit_name(self) -> &'static str {
        match self {
            Self::Socket => "slipforkd.socket",
            Self::Service => "slipforkd.service",
        }
    }
}

/// The production handover budget.
pub const DAEMON_HANDOVER: HandoverBudget = HandoverBudget {
    grace: DAEMON_GRACE,
    poll: DAEMON_POLL,
};

/// What one probe of `<runtime_dir>/daemon.sock` found.
///
/// `Absent` is the only "released" answer, and the handover re-probes `UnitInstaller::is_served`
/// after `Absent` and `Unprovable` before it trusts that: a probe that could not name a holder has
/// proven nothing, so an answering socket is still a held socket.
#[derive(Debug)]
pub enum SocketHolder {
    /// A process answers and is proven to be this host's `slipforkd`; `pidfd` pins that exact
    /// process, so a later signal can never reach a reused pid.
    ProvenSlipforkd {
        /// Peer pid the ownership proof named.
        pid: u32,
        /// `pidfd_open(2)` descriptor for that process, opened at proof time.
        pidfd: OwnedFd,
    },
    /// A named process answers, but its executable basename is not `slipforkd`.
    Foreign {
        /// Peer pid from `SO_PEERCRED`.
        pid: u32,
    },
    /// A process answers, but no pid this namespace can address could be read from it
    /// (`SO_PEERCRED` reported `0`) or its executable could not be inspected.
    Unprovable,
    /// Nothing answers the socket (a leftover socket file is not a holder).
    Absent,
}

/// Release `<runtime_dir>/daemon.sock` from any holder before the installed units start.
///
/// One function, one order, every step fail-closed (`ADR-0011` §3):
///
/// 1. the units this installer writes are stopped while they exist;
/// 2. a daemon that speaks the typed contract is asked to shut down, then awaited (bounded);
/// 3. whatever still answers must be proven to be a `slipforkd` by the adapter's
///    `UnitInstaller::socket_holder` probe — peer pid from `SO_PEERCRED`, `/proc/<pid>/exe`
///    basename, and a `pidfd` that pins that process — and is then `SIGTERM`ed through the pidfd,
///    with `SIGKILL` only after the grace window;
/// 4. a holder that cannot be proven is refused in one line: it is never signalled, and the socket
///    is re-probed so an answering socket never lets the install proceed.
///
/// Every probe and the shutdown go through `installer`, so one adapter sees one socket at a time.
/// No holder means no effect beyond the unit stop, so a second run converges with `[ok] unchanged`.
pub fn release_daemon(
    installer: &UnitInstaller,
    budget: HandoverBudget,
    runtime_dir: &Path,
) -> Result<(), String> {
    let socket = DaemonControlServer::socket_path_in(runtime_dir);

    installer.stop_units(&[ManagedUnit::Socket, ManagedUnit::Service])?;

    // A leftover socket file nobody bound is not a holder: only a successful connect counts.
    if !installer.is_served(runtime_dir)? {
        return Ok(());
    }

    // The typed contract first: ask the daemon to stop itself, then await its disappearance.
    let answered_shutdown = installer.shutdown(runtime_dir).is_ok();
    if answered_shutdown && await_release(installer, budget, runtime_dir)? {
        return Ok(());
    }

    // Anything still answering is proven by the adapter before a signal exists to send. The proof
    // opens a pidfd, so every signal below addresses that process and never a reused pid.
    let holder = installer.socket_holder(runtime_dir)?;
    let (pid, owner) = owner_of(&holder);
    let mut signals_sent = 0;
    loop {
        let facts = HolderFacts {
            socket: socket.clone(),
            pid,
            owner: owner.clone(),
            answered_shutdown,
            released: await_release(installer, budget, runtime_dir)?,
            signals_sent,
        };
        match hold_outcome(&facts) {
            HoldOutcome::Release => return Ok(()),
            HoldOutcome::Refuse(line) => return Err(line),
            HoldOutcome::Terminate(signal) => {
                if let SocketHolder::ProvenSlipforkd { pid: pinned, pidfd } = &holder
                    && let Err(err) = installer.signal_proven(pidfd, signal)
                {
                    // The pinned process exited between the proof and the signal; the bounded
                    // await is still the single judge of whether the socket was released.
                    if await_release(installer, budget, runtime_dir)? {
                        return Ok(());
                    }
                    return Err(held_socket(
                        &socket,
                        *pinned,
                        &format!("could not be signalled ({err})"),
                    ));
                }
                signals_sent += 1;
            }
        }
    }
}

/// Who the kernel says answers `<runtime_dir>/daemon.sock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerOwner {
    /// A live process that is not this host's `slipforkd`.
    Foreign { pid: u32 },
    /// A proven `slipforkd`.
    Daemon { pid: u32 },
    /// A peer this PID namespace cannot name (or a probe that named none).
    Unprovable,
}

/// Every fact the handover decision depends on — plain values, no socket, no clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderFacts {
    /// The control socket the refusal names.
    pub socket: PathBuf,
    /// The peer pid, when one was named.
    pub pid: Option<u32>,
    /// What the ownership proof reported.
    pub owner: PeerOwner,
    /// Whether the holder accepted `DaemonRequest::Shutdown`.
    pub answered_shutdown: bool,
    /// Whether the bounded wait saw the socket released.
    pub released: bool,
    /// Signals already delivered through the pinned pidfd.
    pub signals_sent: usize,
}

/// The one decision the handover makes once its facts are gathered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldOutcome {
    /// Nothing holds the socket (or the shutdown was honoured).
    Release,
    /// Refuse in one line; the holder is never signalled.
    Refuse(String),
    /// Signal the pinned holder through its pidfd.
    Terminate(ProcessSignal),
}

/// Decide the handover's next step from plain facts (`ADR-0011` §3, invariant `1d`).
///
/// Pure: it reads no socket, clock, or database, so every refusal line and the
/// `SIGTERM` → `SIGKILL` order are pinned by deterministic tests.
pub fn hold_outcome(facts: &HolderFacts) -> HoldOutcome {
    if facts.released {
        return HoldOutcome::Release;
    }
    match facts.owner {
        PeerOwner::Daemon { pid } => match facts.signals_sent {
            0 => HoldOutcome::Terminate(ProcessSignal::Terminate),
            1 => HoldOutcome::Terminate(ProcessSignal::Kill),
            _ => HoldOutcome::Refuse(held_socket(
                &facts.socket,
                pid,
                "still holds the socket after `SIGTERM` and `SIGKILL`",
            )),
        },
        PeerOwner::Foreign { pid } => HoldOutcome::Refuse(held_socket(
            &facts.socket,
            pid,
            "is not a slipforkd this install can prove and stop",
        )),
        PeerOwner::Unprovable => {
            // Naming `sudo kill 0` would be worse than naming none.
            let reason = if facts.answered_shutdown {
                "answers the socket, but the probe could not name its holder"
            } else {
                "answers the socket, but this install cannot prove which process holds it"
            };
            HoldOutcome::Refuse(format!(
                "{} {reason}; stop it manually with `sudo systemctl stop slipforkd.socket slipforkd.service`",
                facts.socket.display()
            ))
        }
    }
}

/// Map one probe answer onto the facts the pure decision consumes.
fn owner_of(holder: &SocketHolder) -> (Option<u32>, PeerOwner) {
    match holder {
        SocketHolder::ProvenSlipforkd { pid, .. } => (Some(*pid), PeerOwner::Daemon { pid: *pid }),
        SocketHolder::Foreign { pid } => (Some(*pid), PeerOwner::Foreign { pid: *pid }),
        SocketHolder::Unprovable | SocketHolder::Absent => (None, PeerOwner::Unprovable),
    }
}

/// Probe `<runtime_dir>/daemon.sock` and prove who answers it (`SO_PEERCRED` + `/proc/<pid>/exe`).
///
/// This is the single place kernel credentials become an ownership fact: the pid comes from
/// `SO_PEERCRED`, the executable **basename** from `/proc/<pid>/exe` (an upgrade leaves
/// `slipforkd (deleted)` under a still-running daemon), and a proven holder is pinned with a
/// [`pidfd`](open_pidfd) so a later signal can never reach a reused pid.
///
/// `Absent` means the connect failed; it is **not** a release decision — the handover re-probes
/// `UnitInstaller::is_served` after `Absent` and `Unprovable`, so a probe that could not name a
/// holder can never let a held socket through.
pub fn probe_socket_holder(runtime_dir: &Path) -> Result<SocketHolder, String> {
    let socket = DaemonControlServer::socket_path_in(runtime_dir);
    let stream = match UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(SocketHolder::Absent);
        }
        Err(err) => return Err(format!("cannot connect to {}: {err}", socket.display())),
    };
    let Ok(pid) = peer_pid(&stream) else {
        // A peer outside this PID namespace reports pid 0: it answers, but nothing here can be
        // proven about it.
        return Ok(SocketHolder::Unprovable);
    };
    let executable = match fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(executable) => executable,
        // It exited between the connect and the read: the socket is re-probed before any action.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SocketHolder::Absent),
        Err(_) => return Ok(SocketHolder::Unprovable),
    };
    let name = executable
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let basename = name.strip_suffix(" (deleted)").unwrap_or(name);
    if basename != "slipforkd" {
        return Ok(SocketHolder::Foreign { pid });
    }
    let pidfd =
        open_pidfd(pid).map_err(|err| format!("cannot pin the slipforkd pid {pid}: {err}"))?;
    Ok(SocketHolder::ProvenSlipforkd { pid, pidfd })
}

/// Await the control socket's disappearance, bounded by the caller's grace window.
fn await_release(
    installer: &UnitInstaller,
    budget: HandoverBudget,
    runtime_dir: &Path,
) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + budget.grace;
    loop {
        if !installer.is_served(runtime_dir)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(budget.poll);
    }
}
impl UnitInstaller {
    /// `systemctl stop` every unit that exists in `units` (a missing unit is skipped).
    ///
    /// Runs before `enable --now`, so `systemctl start slipforkd.service` never races a
    /// unit-managed daemon of an older build for the control socket.
    pub(crate) fn stop_units(&self, units: &[ManagedUnit]) -> Result<(), String> {
        for unit in units {
            let name = unit.unit_name();
            // `LoadState=not-found` is a unit this installer never wrote; every other state is a
            // unit the host knows, and `systemctl stop` is a no-op when it is already inactive.
            let load_state = self.run_capture(&[
                OsStr::new("show"),
                OsStr::new("--property=LoadState"),
                OsStr::new("--value"),
                OsStr::new(name),
            ])?;
            if load_state.trim() == "not-found" {
                continue;
            }
            self.run_tool(&[OsStr::new("stop"), OsStr::new(name)])?;
        }
        Ok(())
    }

    /// `true` when a process answers `<runtime_dir>/daemon.sock` (one connect, dropped at once).
    pub(crate) fn is_served(&self, runtime_dir: &Path) -> Result<bool, String> {
        Ok(DaemonControlClient::bind(runtime_dir).is_served())
    }

    /// One `DaemonRequest::Shutdown` round trip against `<runtime_dir>/daemon.sock`.
    pub(crate) fn shutdown(&self, runtime_dir: &Path) -> Result<(), String> {
        DaemonControlClient::bind(runtime_dir)
            .shutdown()
            .map_err(|err| err.to_string())
    }

    /// The one probe that proves (or refuses) who answers the socket.
    pub(crate) fn socket_holder(&self, runtime_dir: &Path) -> Result<SocketHolder, String> {
        probe_socket_holder(runtime_dir)
    }

    /// `pidfd_send_signal(2)` the process pinned by a [`SocketHolder::ProvenSlipforkd`] proof.
    pub(crate) fn signal_proven(
        &self,
        pidfd: &OwnedFd,
        signal: ProcessSignal,
    ) -> Result<(), String> {
        signal_pidfd(pidfd.as_fd(), signal)
            .map_err(|err| format!("cannot signal the proven slipforkd: {err}"))
    }
}

/// One line naming the socket, its holder, and the exact manual command (invariant `1d`).
fn held_socket(socket: &Path, pid: u32, reason: &str) -> String {
    format!(
        "{} is held by pid {pid}, which {reason}; stop it manually with `sudo kill {pid}`",
        socket.display()
    )
}
